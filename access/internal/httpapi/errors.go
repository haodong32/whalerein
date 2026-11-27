package httpapi

import (
	"context"
	"errors"
	"net/http"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/relay"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/selfstatus"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

var (
	errRouteNotFound        = errors.New("route not found")
	errMethod               = errors.New("method not allowed")
	errInvalidBody          = errors.New("invalid request body")
	errInvalidPagination    = errors.New("invalid pagination parameters")
	errUnsupportedMediaType = errors.New("unsupported content type")
	errBodyTooLarge         = errors.New("request body too large")
	errMissingAuth          = errors.New("missing bearer token")
	errDirectoryNotFound    = errors.New("host directory not found")
	errDirectoryUnavailable = errors.New("host directory unavailable")
	errInternalToken        = errors.New("invalid internal callback token")
	errPanic                = errors.New("handler panic")
	errSelfStatusDisabled   = errors.New("self-status relay is not configured")
)

type errorInfo struct {
	status  int
	code    whalereinv1.ErrorCode
	message string
	reason  string
}

func classify(err error) errorInfo {
	var enumErr *whalereinv1.UnknownEnumError
	if errors.As(err, &enumErr) {
		// The field name is schema-defined. The submitted enum name or value is
		// client input and is deliberately not echoed.
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible,
			"protocol version incompatible: unsupported value for " + enumErr.Field, ""}
	}
	denial := authorityDenial(err)
	switch {
	case err == nil:
		return errorInfo{status: http.StatusOK, code: whalereinv1.ErrorCode_error_code_unspecified}
	case errors.Is(err, errRouteNotFound):
		return errorInfo{http.StatusNotFound, whalereinv1.ErrorCode_not_found, "route not found", ""}
	case errors.Is(err, errMethod):
		return errorInfo{http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument, "method not allowed", ""}
	case errors.Is(err, errBodyTooLarge):
		return errorInfo{http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large, "request body too large", ""}
	case errors.Is(err, errUnsupportedMediaType):
		return errorInfo{http.StatusUnsupportedMediaType, whalereinv1.ErrorCode_invalid_argument, "content type must be application/json", ""}
	case errors.Is(err, errInvalidPagination):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, "invalid pagination parameters; limit is 1-500 and offset is 0-1000000", ""}
	case errors.Is(err, errInvalidBody):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, "request body is not valid JSON for this endpoint", ""}
	case errors.Is(err, relay.ErrDisabled):
		return errorInfo{http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability,
			"temporary ciphertext storage is not configured on this Access deployment", ""}
	case errors.Is(err, relay.ErrEpoch):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible, "relay epoch incompatible", ""}
	case errors.Is(err, relay.ErrInvalid):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, "invalid relay request", ""}
	case errors.Is(err, relay.ErrUnauthorized):
		return errorInfo{http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized, "unauthorized", ""}
	case errors.Is(err, relay.ErrForbidden):
		return errorInfo{http.StatusForbidden, whalereinv1.ErrorCode_forbidden, "forbidden", ""}
	case errors.Is(err, relay.ErrNotFound):
		return errorInfo{http.StatusNotFound, whalereinv1.ErrorCode_not_found, "relay object not found", ""}
	case errors.Is(err, relay.ErrWrongOwner):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "wrong owner node", "wrong_owner_path"}
	case errors.Is(err, relay.ErrDigestMismatch):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_payload_digest_mismatch, "payload digest mismatch", ""}
	case errors.Is(err, relay.ErrConflict):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "relay object conflict", ""}
	case errors.Is(err, relay.ErrExpired):
		return errorInfo{http.StatusGone, whalereinv1.ErrorCode_expired, "relay object expired", ""}
	case errors.Is(err, relay.ErrTooLarge):
		return errorInfo{http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large, "relay object is too large", ""}
	case errors.Is(err, relay.ErrQuota):
		return errorInfo{http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited, "relay quota exceeded", "quota_exceeded"}
	case errors.Is(err, relay.ErrPressure):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "relay storage pressure", "storage_pressure"}
	case errors.Is(err, relay.ErrUnavailable):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "relay directory unavailable", ""}
	case errors.Is(err, errSelfStatusDisabled):
		return errorInfo{http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability,
			"self-status relay is not configured on this Access deployment", ""}
	case errors.Is(err, selfstatus.ErrInvalid):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, "invalid self-status request", ""}
	case errors.Is(err, selfstatus.ErrInaccessible):
		// Deliberately identical to every other absent tuple: an unknown Host, a
		// foreign grant and a Host refusal must not be distinguishable.
		return errorInfo{http.StatusNotFound, whalereinv1.ErrorCode_not_found, "not found", ""}
	case errors.Is(err, selfstatus.ErrReplay):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "self-status exchange already used", ""}
	case errors.Is(err, selfstatus.ErrBusy):
		return errorInfo{http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited,
			"self-status admission limit reached", "admission_limit"}
	case errors.Is(err, selfstatus.ErrHostOffline):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "self-status pending", "host_offline"}
	case errors.Is(err, selfstatus.ErrTimeout), errors.Is(err, selfstatus.ErrHostUnavailable):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "self-status pending", "self_status_pending"}
	case errors.Is(err, errDirectoryNotFound):
		// Invalid, unknown, expired and already-used invitations share one
		// response, so the route discloses nothing about the Host or token.
		return errorInfo{http.StatusNotFound, whalereinv1.ErrorCode_not_found, "not found", ""}
	case errors.Is(err, errDirectoryUnavailable):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "service unavailable", ""}
	case errors.Is(err, errMissingAuth), errors.Is(err, auth.ErrUnauthorized), errors.Is(err, pairing.ErrUnauthorized), errors.Is(err, errInternalToken):
		return errorInfo{http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized, "unauthorized", ""}
	case denial != nil:
		code := whalereinv1.ErrorCode_forbidden
		if denial.StatusCode == http.StatusServiceUnavailable {
			code = whalereinv1.ErrorCode_unavailable
		}
		return errorInfo{denial.StatusCode, code, "forbidden", denial.Reason}
	case errors.Is(err, auth.ErrForbidden), errors.Is(err, pairing.ErrHostRejected):
		return errorInfo{http.StatusForbidden, whalereinv1.ErrorCode_forbidden, "forbidden", ""}
	case errors.Is(err, errAuthorizationExpired):
		return errorInfo{http.StatusGone, whalereinv1.ErrorCode_expired, "authorization expired", ""}
	case errors.Is(err, errExternalNotConfigured):
		// The route exists in every build, so a deployment that did not enable
		// external account mode answers with an explicit capability error
		// instead of a 404 that looks like a mistyped path.
		return errorInfo{http.StatusNotImplemented, whalereinv1.ErrorCode_unsupported_capability,
			"external account mode is not configured on this Access deployment", ""}
	case errors.Is(err, external.ErrEnforcementUnavailable):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "external enforcement unavailable", ""}
	case errors.Is(err, external.ErrAuthorizationMismatch):
		return errorInfo{http.StatusForbidden, whalereinv1.ErrorCode_forbidden, "authorization mismatch", ""}
	case errors.Is(err, errAuthorizationRefused):
		return errorInfo{http.StatusForbidden, whalereinv1.ErrorCode_forbidden, "authorization refused", ""}
	case errors.Is(err, errOperationConflict):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "operation conflict", ""}
	case errors.Is(err, auth.ErrBadRequest), errors.Is(err, pairing.ErrInvalidClaim), errors.Is(err, requests.ErrInvalidRequest):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, "invalid argument", ""}
	case errors.Is(err, auth.ErrNotFound), errors.Is(err, pairing.ErrHostNotFound), errors.Is(err, requests.ErrNotFound), errors.Is(err, store.ErrNotFound):
		return errorInfo{http.StatusNotFound, whalereinv1.ErrorCode_not_found, "not found", ""}
	case errors.Is(err, pairing.ErrPairingCommittedUnconfirmed):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_pairing_committed_unconfirmed, "committed pairing Host confirmation unknown", ""}
	case errors.Is(err, pairing.ErrPairingCommittedUnavailable):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_pairing_committed_unavailable, "committed pairing is no longer available", ""}
	case errors.Is(err, pairing.ErrPairingFenced):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_pairing_fenced, "pairing intent invalidated", ""}
	case errors.Is(err, pairing.ErrTokenUsed):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "pairing token already used", ""}
	case errors.Is(err, pairing.ErrTokenExpired):
		return errorInfo{http.StatusGone, whalereinv1.ErrorCode_expired, "pairing token expired", ""}
	case errors.Is(err, pairing.ErrFailureBudget):
		return errorInfo{http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited, "pairing token failure budget exhausted", ""}
	case errors.Is(err, pairing.ErrHostOffline), errors.Is(err, pairing.ErrHostUnavailable), errors.Is(err, auth.ErrUnavailable), errors.Is(err, requests.ErrUnavailable):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "service unavailable", ""}
	case errors.Is(err, pairing.ErrClaimRequired):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "pairing HELLO is required before FINISH", ""}
	case errors.Is(err, pairing.ErrClaimBusy):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "another claim on this invitation is in progress", ""}
	case errors.Is(err, pairing.ErrProtocolVersion), errors.Is(err, requests.ErrProtocolVersion):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible, "protocol version incompatible", ""}
	case errors.Is(err, requests.ErrDigestMismatch):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_payload_digest_mismatch, "payload digest mismatch", ""}
	case errors.Is(err, requests.ErrConflict), errors.Is(err, store.ErrConflict):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "conflict", ""}
	case errors.Is(err, requests.ErrExpired), errors.Is(err, auth.ErrSignatureExpired):
		return errorInfo{http.StatusGone, whalereinv1.ErrorCode_expired, "request expired", ""}
	case errors.Is(err, requests.ErrForbidden), errors.Is(err, requests.ErrResultMismatch), errors.Is(err, requests.ErrInvalidResult), errors.Is(err, requests.ErrReleaseFenced):
		return errorInfo{http.StatusForbidden, whalereinv1.ErrorCode_forbidden, "forbidden", ""}
	case errors.Is(err, requests.ErrResultTooLarge):
		return errorInfo{http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large, "result body too large", ""}
	case errors.Is(err, requests.ErrRequestTooLarge):
		return errorInfo{http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_payload_too_large, "request body too large", ""}
	case errors.Is(err, context.Canceled):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "request canceled", ""}
	case errors.Is(err, context.DeadlineExceeded):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "request timed out", ""}
	case errors.Is(err, pairing.ErrInternal):
		return errorInfo{http.StatusInternalServerError, whalereinv1.ErrorCode_internal, "internal error", ""}
	default:
		return errorInfo{http.StatusInternalServerError, whalereinv1.ErrorCode_internal, "internal error", ""}
	}
}

func codeName(code whalereinv1.ErrorCode) string {
	if name, ok := whalereinv1.ErrorCode_name[int32(code)]; ok {
		return name
	}
	return "internal"
}

// authorityDenial extracts the specific reason for an ended authority. A
// generic forbidden condition keeps an empty reason and its old mapping.
func authorityDenial(err error) *auth.AuthorityDenial {
	var denial *auth.AuthorityDenial
	if errors.As(err, &denial) {
		return denial
	}
	return nil
}
