package httpapi

import (
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"net/url"
	"regexp"
	"strconv"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/policy"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

// resourceIDPattern is the cross-component identifier contract: the same
// bound the Host runtime applies to MQTT topic ids. Anything else would reach
// SQL as an unbounded value and fail as an internal error.
var resourceIDPattern = regexp.MustCompile(`^[A-Za-z0-9_.:-]{1,128}$`)

const (
	// Host list bounds. The default equals the hard cap so ordinary devices
	// keep one request; a larger grant set pages with limit/offset and sees
	// explicit truncation headers.
	defaultHostListLimit = 500
	maxHostListLimit     = 500
	maxHostListOffset    = 1_000_000
)

func (s *Server) handleInvitation(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	if s.deps.RequireDeviceSession {
		s.writeError(w, r, auth.ErrForbidden)
		return
	}

	var request whalereinv1.PairingInvitationRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	hostID := strings.TrimSpace(request.GetHostId())
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	if token, err := bearerToken(r); err == nil {
		if adminErr := s.deps.Auth.AuthenticateAdmin(token); adminErr != nil {
			s.writeError(w, r, adminErr)
			return
		}
	} else if authErr := s.deps.Auth.AuthenticateHostRequest(r.Context(), auth.HostProof{
		Purpose:        auth.HostAuthInvitation,
		HostID:         hostID,
		SignedAtUnixMs: request.GetSignedAtUnixMs(),
		Signature:      request.GetSignature(),
	}); authErr != nil {
		s.writeError(w, r, authErr)
		return
	}
	invitation, err := s.deps.Pairing.CreateInvitation(r.Context(), hostID)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, invitation)
}

func (s *Server) handleHostRevocation(w http.ResponseWriter, r *http.Request, params map[string]string) {
	hostID := params["host_id"]
	deviceID := params["device_id"]
	if !validResourceID(hostID) || !validResourceID(deviceID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	var request whalereinv1.HostGrantRevocation
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	if err := s.deps.Auth.AuthenticateHostRequest(r.Context(), auth.HostProof{
		Purpose:        auth.HostAuthRevoke,
		HostID:         hostID,
		DeviceID:       deviceID,
		SignedAtUnixMs: request.GetSignedAtUnixMs(),
		Signature:      request.GetSignature(),
	}); err != nil {
		s.writeError(w, r, err)
		return
	}
	if err := s.deps.Auth.RevokeHostGrant(r.Context(), hostID, deviceID); err != nil {
		s.writeError(w, r, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func validResourceID(value string) bool {
	return resourceIDPattern.MatchString(value)
}

// Wire identifier bounds for client-supplied fields that Access persists and
// logs (proto/v1/WIRE.md). A request whose identifier violates them is
// invalid_argument and is never forwarded.
const (
	maxChannelIdentifierBytes = 128
	maxTraceIdentifierBytes   = 64
)

// validWireIdentifier checks one client identifier against its wire bound:
// at most maxBytes, printable ASCII, and without the "|" AAD separator. An
// empty value is valid because the contract defines an absent channel id while
// a channel is being established and an absent client trace id.
func validWireIdentifier(value string, maxBytes int) bool {
	if len(value) > maxBytes {
		return false
	}
	for i := 0; i < len(value); i++ {
		character := value[i]
		if character < 0x21 || character > 0x7e || character == '|' {
			return false
		}
	}
	return true
}

func (s *Server) handleClaim(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	if s.deps.RequireDeviceSession {
		s.writeError(w, r, auth.ErrForbidden)
		return
	}

	var request whalereinv1.PairingClaimRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	response, err := s.deps.Pairing.Claim(r.Context(), &request)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, response)
}

func (s *Server) handleHosts(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	limit, offset, err := hostListBounds(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	views, truncated, err := s.deps.Auth.ListHosts(r.Context(), identity, limit, offset)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	list := &whalereinv1.HostList{Hosts: make([]*whalereinv1.HostInfo, 0, len(views))}
	for _, view := range views {
		list.Hosts = append(list.Hosts, &whalereinv1.HostInfo{
			HostId:          view.HostID,
			Online:          view.Online,
			ProtocolVersion: view.ProtocolVersion,
			Fingerprint:     view.Fingerprint,
		})
	}
	// The wire message has no page fields, so the response stays bounded and
	// reports the page in headers. A client that sees Truncated: true must
	// continue with offset+limit.
	w.Header().Set("X-Whalerein-Hosts-Limit", strconv.Itoa(limit))
	w.Header().Set("X-Whalerein-Hosts-Offset", strconv.Itoa(offset))
	w.Header().Set("X-Whalerein-Hosts-Returned", strconv.Itoa(len(list.Hosts)))
	w.Header().Set("X-Whalerein-Hosts-Truncated", strconv.FormatBool(truncated))
	s.writeProto(w, http.StatusOK, list)
}

// handleHostDirectory returns Host display metadata before pairing. The bearer
// value is the one-time invitation token, never a device or admin credential.
// Every invalid, unknown, expired or already-used invitation maps to the same
// 404 so the route cannot be used to probe host existence or token state.
func (s *Server) handleHostDirectory(w http.ResponseWriter, r *http.Request, params map[string]string) {
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	token, err := bearerToken(r)
	if err != nil {
		s.writeError(w, r, errDirectoryNotFound)
		return
	}
	if s.deps.Directory == nil {
		s.writeError(w, r, errDirectoryUnavailable)
		return
	}
	directory, err := s.deps.Directory.HostDirectory(r.Context(), hostID, pairing.HashInvitationToken(token), time.Now().UTC())
	if err != nil {
		if errors.Is(err, store.ErrNotFound) {
			s.writeError(w, r, errDirectoryNotFound)
			return
		}
		s.writeError(w, r, errDirectoryUnavailable)
		return
	}
	// The invitation is one-time and the response is Host-owned metadata. No
	// intermediary may cache it past the token's single use.
	w.Header().Set("Cache-Control", "no-store")
	s.writeProto(w, http.StatusOK, &whalereinv1.HostDirectory{
		HostId:      directory.HostID,
		DisplayName: directory.DisplayName,
		OsFamily:    directory.OSFamily,
	})
}

func hostListBounds(r *http.Request) (int, int, error) {
	query := r.URL.Query()
	limit := defaultHostListLimit
	if raw := query.Get("limit"); raw != "" {
		parsed, err := strconv.Atoi(raw)
		if err != nil || parsed < 1 || parsed > maxHostListLimit {
			return 0, 0, errInvalidPagination
		}
		limit = parsed
	}
	offset := 0
	if raw := query.Get("offset"); raw != "" {
		parsed, err := strconv.Atoi(raw)
		if err != nil || parsed < 0 || parsed > maxHostListOffset {
			return 0, 0, errInvalidPagination
		}
		offset = parsed
	}
	return limit, offset, nil
}

func (s *Server) handleSubmit(w http.ResponseWriter, r *http.Request, params map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	if err := s.deps.Auth.AuthorizeHost(r.Context(), identity, hostID); err != nil {
		s.writeError(w, r, err)
		return
	}
	var request whalereinv1.HostRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	// Client-generated identities become durable keys and later path
	// parameters, so they obey the same bounded contract as route ids.
	if !validResourceID(request.GetRequestId()) ||
		(request.GetCommandId() != "" && !validResourceID(request.GetCommandId())) ||
		!validWireIdentifier(request.GetChannelId(), maxChannelIdentifierBytes) ||
		!validWireIdentifier(request.GetTraceId(), maxTraceIdentifierBytes) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	if request.GetHostId() != "" && request.GetHostId() != hostID {
		s.writeError(w, r, fmt.Errorf("%w: host_id differs from path", requests.ErrInvalidRequest))
		return
	}
	if request.GetDeviceId() != "" && request.GetDeviceId() != identity.DeviceID {
		s.writeError(w, r, requests.ErrForbidden)
		return
	}
	request.HostId = hostID
	request.DeviceId = identity.DeviceID
	if request.GetTraceId() == "" {
		// The HTTP correlation id is bounded more loosely than the wire
		// trace_id; never copy a value that would violate the wire bound.
		if meta := requestMetaFrom(r.Context()); meta != nil && len(meta.traceID) <= maxTraceIdentifierBytes {
			request.TraceId = meta.traceID
		}
	}
	row, _, err := s.deps.Requests.Submit(r.Context(), requests.SubmitInput{
		DeviceID:  identity.DeviceID,
		SessionID: identity.SessionID,
		HostID:    hostID,
		Request:   &request,
	})
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	s.writeProto(w, http.StatusOK, &whalereinv1.RequestReceipt{
		RequestId:       row.RequestID,
		Status:          row.Status,
		CommandId:       row.CommandID,
		ExpiresAtUnixMs: row.ExpiresAt.UnixMilli(),
	})
}

func (s *Server) handleGetRequest(w http.ResponseWriter, r *http.Request, params map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	requestID := params["request_id"]
	if !validResourceID(requestID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	// One durable decision authorizes ownership, the active grant revisions and
	// the result bound. No second authorization call can race a revocation
	// between the decision and the response write.
	row, err := s.deps.Requests.Release(r.Context(), identity.DeviceID, identity.SessionID, requestID,
		requests.ResultValueLimit(s.deps.MaxBodyBytes))
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	// A status read is observational for execution liveness: it never renews
	// the request lease or extends expires_at. Only an explicit heartbeat
	// sustains a live one-shot command.
	state := &whalereinv1.RequestState{
		RequestId:       row.RequestID,
		HostId:          row.HostID,
		DeviceId:        row.DeviceID,
		Status:          row.Status,
		CommandId:       row.CommandID,
		PayloadDigest:   row.PayloadDigest,
		ResultBody:      row.ResultBody,
		ErrorCode:       row.ErrorCode,
		ErrorMessage:    row.ErrorMessage,
		ExpiresAtUnixMs: row.ExpiresAt.UnixMilli(),
		UpdatedAtUnixMs: row.UpdatedAt.UnixMilli(),
		ChannelId:       row.ChannelID,
	}
	if len(row.ResultReference) > 0 {
		var reference whalereinv1.PayloadReference
		if err := proto.Unmarshal(row.ResultReference, &reference); err == nil {
			state.ResultReference = &reference
		}
	}
	s.writeProto(w, http.StatusOK, state)
}

func (s *Server) handleHeartbeat(w http.ResponseWriter, r *http.Request, params map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	requestID := params["request_id"]
	if !validResourceID(requestID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	row, err := s.deps.Requests.Get(r.Context(), identity.DeviceID, requestID)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	if identity.SessionID != "" {
		if err := s.deps.Auth.AuthorizeHost(r.Context(), identity, row.HostID); err != nil {
			s.writeError(w, r, err)
			return
		}
	}
	// The store takes the row lock and renews only the exact live,
	// non-terminal, unexpired request. A non-renewal is a typed 404/410/409,
	// never a silent 204.
	if _, err := s.deps.Requests.Heartbeat(r.Context(), identity.DeviceID, identity.SessionID, requestID); err != nil {
		s.writeError(w, r, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleDeleteDevice(w http.ResponseWriter, r *http.Request, params map[string]string) {
	deviceID := params["device_id"]
	if !validResourceID(deviceID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	token, err := bearerToken(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	if adminErr := s.deps.Auth.AuthenticateAdmin(token); adminErr != nil {
		identity, deviceErr := s.deps.Auth.AuthenticateDeviceToken(r.Context(), token)
		if deviceErr != nil {
			s.writeError(w, r, deviceErr)
			return
		}
		if identity.DeviceID != deviceID {
			s.writeError(w, r, auth.ErrForbidden)
			return
		}
	}
	if err := s.deps.Auth.RevokeDevice(r.Context(), deviceID); err != nil {
		s.writeError(w, r, err)
		return
	}
	w.WriteHeader(http.StatusNoContent)
}

// handleHostAuthority renews one observation lease. It returns 204 while the
// Device's grant, session and Host enrollment are all current, and a typed
// authority reason when any of them ended. A client treats that reason as
// terminal; the encrypted observation stream cannot carry an Access-authored
// close, so this lease check is how authority termination is observed even
// while the stream is idle.
func (s *Server) handleHostAuthority(w http.ResponseWriter, r *http.Request, params map[string]string) {
	identity, err := s.requireDevice(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	if err := s.deps.Auth.AuthorizeHost(r.Context(), identity, hostID); err != nil {
		s.writeError(w, r, err)
		return
	}
	w.Header().Set("Cache-Control", "no-store")
	w.WriteHeader(http.StatusNoContent)
}

func (s *Server) handleHealth(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	diagnostics.HealthHandler().ServeHTTP(w, r)
}

func (s *Server) handleReady(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	if s.deps.RequireDeviceSession {
		w.Header().Set("WhaleRein-Session-Authority-Revision", s.deps.SessionAuthorityRevision)
	}
	s.deps.Readiness.ReadyHandler().ServeHTTP(w, r)
}

func (s *Server) handleMetrics(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	s.deps.Metrics.Handler().ServeHTTP(w, r)
}

func (s *Server) requireDevice(r *http.Request) (auth.Identity, error) {
	token, err := bearerToken(r)
	if err != nil {
		return auth.Identity{}, err
	}
	return s.deps.Auth.AuthenticateDeviceToken(r.Context(), token)
}

func bearerToken(r *http.Request) (string, error) {
	header := strings.TrimSpace(r.Header.Get("Authorization"))
	parts := strings.Fields(header)
	if len(parts) != 2 || !strings.EqualFold(parts[0], "Bearer") || parts[1] == "" {
		return "", errMissingAuth
	}
	return parts[1], nil
}

func (s *Server) decodeProto(w http.ResponseWriter, r *http.Request, message proto.Message) error {
	data, err := s.readJSONBody(w, r)
	if err != nil {
		return err
	}
	return decodeJSONMessage(data, message)
}

// readJSONBody applies the shared content-type and body bounds, then returns
// the raw bytes so a wrapper document can be decoded before its embedded
// protobuf message.
func (s *Server) readJSONBody(w http.ResponseWriter, r *http.Request) ([]byte, error) {
	if err := requireJSONContentType(r); err != nil {
		return nil, err
	}
	reader := http.MaxBytesReader(w, r.Body, s.deps.MaxBodyBytes)
	data, err := io.ReadAll(reader)
	if err != nil {
		var maxErr *http.MaxBytesError
		if errors.As(err, &maxErr) {
			return nil, errBodyTooLarge
		}
		return nil, errInvalidBody
	}
	if len(strings.TrimSpace(string(data))) == 0 {
		return nil, errInvalidBody
	}
	return data, nil
}

func decodeJSONMessage(data []byte, message proto.Message) error {
	// DecodeJSON owns the cross-version policy: an additive field from a newer
	// peer is ignored, while an enum value this build does not know is an
	// explicit incompatibility instead of a silent zero value. Syntax and type
	// errors stay distinguishable from that incompatibility.
	if err := whalereinv1.DecodeJSON(data, message); err != nil {
		var enumErr *whalereinv1.UnknownEnumError
		if errors.As(err, &enumErr) {
			return err
		}
		var oneofErr *whalereinv1.MissingOneofError
		if errors.As(err, &oneofErr) {
			return fmt.Errorf("%w: a required message variant is missing", errInvalidBody)
		}
		return errInvalidBody
	}
	return nil
}

// requireJSONContentType enforces the documented JSON body contract. A missing
// or different media type fails with 415 before any parsing, so a version
// mismatch or a form-encoded retry is distinguishable from malformed JSON.
func requireJSONContentType(r *http.Request) error {
	raw := strings.TrimSpace(r.Header.Get("Content-Type"))
	if raw == "" {
		return errUnsupportedMediaType
	}
	mediaType, _, err := mime.ParseMediaType(raw)
	if err != nil || mediaType != "application/json" {
		return errUnsupportedMediaType
	}
	return nil
}

func (s *Server) writeProto(w http.ResponseWriter, status int, message proto.Message) {
	data, err := (protojson.MarshalOptions{EmitUnpopulated: false}).Marshal(message)
	if err != nil {
		s.deps.Logger.Error("http response encode failed")
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte(`{"code":"internal","message":"internal error"}`))
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(data)
}

// writeJSON encodes one small non-protobuf response document.
func (s *Server) writeJSON(w http.ResponseWriter, status int, value any) {
	data, err := json.Marshal(value)
	if err != nil {
		s.deps.Logger.Error("http JSON response encode failed")
		w.Header().Set("Content-Type", "application/json")
		w.WriteHeader(http.StatusInternalServerError)
		_, _ = w.Write([]byte(`{"code":"internal","message":"internal error"}`))
		return
	}
	w.Header().Set("Content-Type", "application/json")
	w.WriteHeader(status)
	_, _ = w.Write(data)
}

func (s *Server) writeError(w http.ResponseWriter, r *http.Request, err error) {
	info := classify(err)
	meta := requestMetaFrom(r.Context())
	if info.status >= 500 && s.deps.Logger != nil {
		// A 500 can carry raw SQL text and identifiers. Log the routed
		// identifiers only in their salted form and strip control characters so
		// the message cannot forge a log line.
		s.deps.Logger.Error("http handler error",
			"status", info.status,
			"error", s.deps.Masker.Redact(err.Error(), s.redactions(meta)))
	}
	if meta != nil && meta.errorCode == "" {
		meta.errorCode = codeName(info.code)
	}
	trace := ""
	requestID := ""
	if meta != nil {
		trace = meta.traceID
		requestID = meta.requestID
	}
	s.writeProto(w, info.status, &whalereinv1.ErrorResponse{
		Code:      info.code,
		Message:   info.message,
		TraceId:   trace,
		RequestId: requestID,
		Reason:    info.reason,
	})
}

// redactions maps every identifier that reached this handler to its salted
// diagnostic form. It is the only channel by which raw identifiers may appear
// in a log message.
func (s *Server) redactions(meta *requestMeta) map[string]string {
	if meta == nil || len(meta.params) == 0 {
		return nil
	}
	out := make(map[string]string, len(meta.params))
	for name, value := range meta.params {
		if value == "" {
			continue
		}
		switch name {
		case "host_id":
			out[value] = s.deps.Masker.Host(value)
		case "device_id":
			out[value] = s.deps.Masker.Device(value)
		case "request_id":
			out[value] = s.deps.Masker.Request(value)
		case "operation_id":
			out[value] = s.deps.Masker.Request(value)
		}
	}
	return out
}

// handleBrokerConnect answers one deferred Mosquitto CONNECT validation. The
// response is the versioned broker policy for this connection: it carries the
// principal, its complete local ACL set and the monotonic policy revision.
func (s *Server) handleBrokerConnect(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	// Read at most the 64 KiB contract bound plus one byte so an oversized
	// request is detected without buffering the whole body. The response stays
	// the versioned text protocol, so a plain deny is written instead of an
	// HTTP 413.
	body, err := io.ReadAll(io.LimitReader(r.Body, (64<<10)+1))
	if err != nil || len(body) > 64<<10 {
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny malformed_request\n"))
		return
	}
	form, err := url.ParseQuery(string(body))
	if err != nil {
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny malformed_request\n"))
		return
	}
	clientID := form.Get("clientid")
	username := form.Get("username")
	password := form.Get("password")
	will := form.Get("will")
	identity, err := s.deps.Auth.AuthenticateBroker(r.Context(), clientID, username, password)
	if err != nil {
		s.deps.Logger.Debug("broker authentication denied")
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny unauthorized\n"))
		return
	}
	connection, err := s.deps.Auth.BrokerPolicy(r.Context(), identity)
	if err != nil {
		s.deps.Logger.Warn("broker policy projection failed", "error", err.Error())
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny unavailable\n"))
		return
	}
	if !connection.WillAllowed(will) {
		writeBrokerPolicy(w, []byte("whalerein/1\ndeny will_not_authorized\n"))
		return
	}
	writeBrokerPolicy(w, policy.Format(connection))
}

func writeBrokerPolicy(w http.ResponseWriter, body []byte) {
	w.Header().Set("Content-Type", "text/plain; charset=utf-8")
	w.WriteHeader(http.StatusOK)
	_, _ = w.Write(body)
}
