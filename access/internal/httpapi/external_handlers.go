package httpapi

import (
	"encoding/json"
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

// HostProofHeader carries the signed JSON Host ownership proof used by the
// proof-scoped enrollment status and credential-replacement endpoints.
const HostProofHeader = "X-Whalerein-Host-Proof"

// Stable errors for the external account endpoints.
var (
	errAuthorizationExpired  = errors.New("authorization expired")
	errAuthorizationRefused  = errors.New("authorization refused")
	errOperationConflict     = errors.New("operation conflict")
	errExternalNotConfigured = errors.New("external account mode is not configured")
)

// handleEnrollHost serves O3: it verifies a portable authorization and the
// Host's proof of possession before committing one enrollment.
func (s *Server) handleEnrollHost(w http.ResponseWriter, r *http.Request, _ map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	var request whalereinv1.HostEnrollmentRequest
	if err := s.decodeProto(w, r, &request); err != nil {
		s.writeError(w, r, err)
		return
	}
	response, err := s.deps.External.EnrollHost(r.Context(), &request)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, response)
}

// handleEnrollmentStatus serves O4 and never returns a credential.
func (s *Server) handleEnrollmentStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	if err := s.externalReadAuth(r); err != nil {
		s.writeError(w, r, err)
		return
	}
	status, err := s.deps.External.EnrollmentStatus(r.Context(), hostID)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, status)
}

// handleOperationStatus serves O6 and never returns a credential.
func (s *Server) handleOperationStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	operationID := params["operation_id"]
	if !validResourceID(operationID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	if err := s.externalReadAuth(r); err != nil {
		s.writeError(w, r, err)
		return
	}
	status, err := s.deps.External.OperationStatus(r.Context(), operationID)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, status)
}

// handleHostEnrollmentProofStatus returns enrollment state to the original
// Host after it proves possession of the current enrollment signing key. It
// deliberately accepts only the Host proof: a bare host id or the admin bearer
// alone is refused.
func (s *Server) handleHostEnrollmentProofStatus(w http.ResponseWriter, r *http.Request, params map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	proof, err := requireHostProof(r)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	status, err := s.deps.External.EnrollmentStatusForHost(r.Context(), hostID, proof)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeProto(w, http.StatusOK, status)
}

// handleCredentialReplacement replaces one enrollment credential after the
// Host proof and a fresh portable authorization are both verified. The new
// credential is delivered exactly once.
func (s *Server) handleCredentialReplacement(w http.ResponseWriter, r *http.Request, params map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	proof, err := requireHostProof(r)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	data, err := s.readJSONBody(w, r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	var envelope struct {
		Authorization json.RawMessage `json:"authorization"`
	}
	if err := json.Unmarshal(data, &envelope); err != nil || len(envelope.Authorization) == 0 {
		s.writeError(w, r, errInvalidBody)
		return
	}
	authorization := &whalereinv1.ExternalAuthorization{}
	if err := decodeJSONMessage(envelope.Authorization, authorization); err != nil {
		s.writeError(w, r, err)
		return
	}
	replacement, err := s.deps.External.RecoverCredential(r.Context(), hostID, authorization, proof)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeJSON(w, http.StatusOK, credentialReplacementResponse{
		Credential: issuedCredentialResponse{
			HostID:   replacement.Credential.HostID,
			MQTTURL:  replacement.Credential.MQTTURL,
			Username: replacement.Credential.Username,
			Password: replacement.Credential.Password,
		},
		State:    replacement.State,
		Revision: replacement.Revision,
	})
}

// handleDisableHostEnrollment durably disables one Host enrollment. It is an
// administrative operation and requires the deployment admin bearer.
func (s *Server) handleDisableHostEnrollment(w http.ResponseWriter, r *http.Request, params map[string]string) {
	if s.deps.External == nil {
		s.writeError(w, r, errExternalNotConfigured)
		return
	}
	hostID := params["host_id"]
	if !validResourceID(hostID) {
		s.writeError(w, r, auth.ErrBadRequest)
		return
	}
	token, err := bearerToken(r)
	if err != nil {
		s.writeError(w, r, err)
		return
	}
	if err := s.deps.Auth.AuthenticateAdmin(token); err != nil {
		s.writeError(w, r, err)
		return
	}
	disabledAt, err := s.deps.External.DisableHost(r.Context(), hostID)
	if err != nil {
		s.writeError(w, r, classifyExternal(err))
		return
	}
	s.writeJSON(w, http.StatusOK, disableEnrollmentResponse{
		State:      "disabled",
		DisabledAt: disabledAt.UTC().Format(time.RFC3339),
	})
}

// requireHostProof parses the signed JSON proof header. A missing header is an
// authentication failure; a malformed one is an authorization refusal.
func requireHostProof(r *http.Request) (external.HostOwnershipProof, error) {
	raw := strings.TrimSpace(r.Header.Get(HostProofHeader))
	if raw == "" {
		return external.HostOwnershipProof{}, errMissingAuth
	}
	proof, err := external.ParseHostOwnershipProof(raw)
	if err != nil {
		return external.HostOwnershipProof{}, external.ErrProofInvalid
	}
	return proof, nil
}

type disableEnrollmentResponse struct {
	State      string `json:"state"`
	DisabledAt string `json:"disabledAt"`
}

type issuedCredentialResponse struct {
	HostID   string `json:"hostId"`
	MQTTURL  string `json:"mqttUrl"`
	Username string `json:"username"`
	Password string `json:"password"`
}

type credentialReplacementResponse struct {
	Credential issuedCredentialResponse `json:"credential"`
	State      string                   `json:"state"`
	Revision   string                   `json:"revision"`
}

// externalReadAuth accepts the admin bearer for status reads.
//
// The contract also admits a Device bearer holding a grant to that Host, or
// the Host's own proof. The proof-scoped Host route exists separately; this
// build keeps the administrative read narrow instead of implying the wider
// device rule.
func (s *Server) externalReadAuth(r *http.Request) error {
	token, err := bearerToken(r)
	if err != nil {
		return err
	}
	return s.deps.Auth.AuthenticateAdmin(token)
}

// classifyExternal maps a refusal to a stable HTTP error.
func classifyExternal(err error) error {
	switch {
	case errors.Is(err, external.ErrAuthorizationExpired):
		return errAuthorizationExpired
	case errors.Is(err, external.ErrAuthorizationMismatch),
		errors.Is(err, external.ErrAuthorizationSignature),
		errors.Is(err, external.ErrProofInvalid),
		errors.Is(err, external.ErrProofExpired),
		errors.Is(err, external.ErrProofReplay),
		errors.Is(err, external.ErrKeyNotValid),
		errors.Is(err, external.ErrEnrollmentDisabled):
		return errAuthorizationRefused
	case errors.Is(err, external.ErrOperationConflict),
		errors.Is(err, store.ErrPairingFenced),
		errors.Is(err, store.ErrOperationConflict),
		errors.Is(err, store.ErrConflict),
		errors.Is(err, external.ErrStaleRevision):
		return errOperationConflict
	case errors.Is(err, external.ErrEnforcementUnavailable):
		return auth.ErrUnavailable
	case errors.Is(err, store.ErrNotFound), errors.Is(err, external.ErrNotFound):
		return errRouteNotFound
	default:
		return err
	}
}
