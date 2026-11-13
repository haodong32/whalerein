package httpapi

import (
	"context"
	"errors"
	"net/http"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

var (
	errRouteNotFound = errors.New("route not found")
	errMethod        = errors.New("method not allowed")
	errInvalidBody   = errors.New("invalid request body")
	errBodyTooLarge  = errors.New("request body too large")
	errMissingAuth   = errors.New("missing bearer token")
	errInternalToken = errors.New("invalid internal callback token")
	errPanic         = errors.New("handler panic")
)

type errorInfo struct {
	status  int
	code    whalereinv1.ErrorCode
	message string
}

func classify(err error) errorInfo {
	switch {
	case err == nil:
		return errorInfo{status: http.StatusOK, code: whalereinv1.ErrorCode_error_code_unspecified}
	case errors.Is(err, errRouteNotFound):
		return errorInfo{http.StatusNotFound, whalereinv1.ErrorCode_not_found, "route not found"}
	case errors.Is(err, errMethod):
		return errorInfo{http.StatusMethodNotAllowed, whalereinv1.ErrorCode_invalid_argument, "method not allowed"}
	case errors.Is(err, errBodyTooLarge):
		return errorInfo{http.StatusRequestEntityTooLarge, whalereinv1.ErrorCode_invalid_argument, "request body too large"}
	case errors.Is(err, errInvalidBody):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, "invalid request body"}
	case errors.Is(err, errMissingAuth), errors.Is(err, auth.ErrUnauthorized), errors.Is(err, pairing.ErrUnauthorized), errors.Is(err, errInternalToken):
		return errorInfo{http.StatusUnauthorized, whalereinv1.ErrorCode_unauthorized, "unauthorized"}
	case errors.Is(err, auth.ErrForbidden), errors.Is(err, pairing.ErrHostRejected):
		return errorInfo{http.StatusForbidden, whalereinv1.ErrorCode_forbidden, "forbidden"}
	case errors.Is(err, auth.ErrBadRequest), errors.Is(err, pairing.ErrInvalidClaim), errors.Is(err, requests.ErrInvalidRequest):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_invalid_argument, "invalid argument"}
	case errors.Is(err, auth.ErrNotFound), errors.Is(err, pairing.ErrHostNotFound), errors.Is(err, requests.ErrNotFound), errors.Is(err, store.ErrNotFound):
		return errorInfo{http.StatusNotFound, whalereinv1.ErrorCode_not_found, "not found"}
	case errors.Is(err, pairing.ErrPairingCommittedUnconfirmed):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_pairing_committed_unconfirmed, "committed pairing Host confirmation unknown"}
	case errors.Is(err, pairing.ErrPairingCommittedUnavailable):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_pairing_committed_unavailable, "committed pairing is no longer available"}
	case errors.Is(err, pairing.ErrPairingFenced):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_pairing_fenced, "pairing intent invalidated"}
	case errors.Is(err, pairing.ErrTokenUsed):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "pairing token already used"}
	case errors.Is(err, pairing.ErrTokenExpired):
		return errorInfo{http.StatusGone, whalereinv1.ErrorCode_expired, "pairing token expired"}
	case errors.Is(err, pairing.ErrFailureBudget):
		return errorInfo{http.StatusTooManyRequests, whalereinv1.ErrorCode_rate_limited, "pairing token failure budget exhausted"}
	case errors.Is(err, pairing.ErrHostOffline), errors.Is(err, pairing.ErrHostUnavailable), errors.Is(err, auth.ErrUnavailable), errors.Is(err, requests.ErrUnavailable):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "service unavailable"}
	case errors.Is(err, pairing.ErrClaimRequired):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "pairing HELLO is required before FINISH"}
	case errors.Is(err, pairing.ErrProtocolVersion), errors.Is(err, requests.ErrProtocolVersion):
		return errorInfo{http.StatusBadRequest, whalereinv1.ErrorCode_protocol_version_incompatible, "protocol version incompatible"}
	case errors.Is(err, requests.ErrDigestMismatch):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_payload_digest_mismatch, "payload digest mismatch"}
	case errors.Is(err, requests.ErrConflict), errors.Is(err, store.ErrConflict):
		return errorInfo{http.StatusConflict, whalereinv1.ErrorCode_conflict, "conflict"}
	case errors.Is(err, requests.ErrExpired), errors.Is(err, auth.ErrSignatureExpired):
		return errorInfo{http.StatusGone, whalereinv1.ErrorCode_expired, "request expired"}
	case errors.Is(err, requests.ErrForbidden), errors.Is(err, requests.ErrResultMismatch), errors.Is(err, requests.ErrInvalidResult):
		return errorInfo{http.StatusForbidden, whalereinv1.ErrorCode_forbidden, "forbidden"}
	case errors.Is(err, context.Canceled):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "request canceled"}
	case errors.Is(err, context.DeadlineExceeded):
		return errorInfo{http.StatusServiceUnavailable, whalereinv1.ErrorCode_unavailable, "request timed out"}
	case errors.Is(err, pairing.ErrInternal):
		return errorInfo{http.StatusInternalServerError, whalereinv1.ErrorCode_internal, "internal error"}
	default:
		return errorInfo{http.StatusInternalServerError, whalereinv1.ErrorCode_internal, "internal error"}
	}
}

func codeName(code whalereinv1.ErrorCode) string {
	if name, ok := whalereinv1.ErrorCode_name[int32(code)]; ok {
		return name
	}
	return "internal"
}
