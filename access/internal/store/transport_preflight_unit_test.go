package store

import (
	"context"
	"testing"

	"github.com/jackc/pgx/v5"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestTransportPreflightMarkerCannotRewriteHostOutcomes(t *testing.T) {
	tests := []HostResultUpdate{
		{TransportPreflight: true, Status: whalereinv1.OnlineStatus_succeeded, ErrorCode: whalereinv1.ErrorCode_payload_too_large},
		{TransportPreflight: true, Status: whalereinv1.OnlineStatus_rejected, ErrorCode: whalereinv1.ErrorCode_unavailable},
	}
	for _, update := range tests {
		began := false
		applied, err := applyHostResult(context.Background(), func(context.Context) (pgx.Tx, error) { began = true; return nil, nil }, update)
		if err == nil || applied || began {
			t.Fatal("invalid preflight was allowed to reach persistence")
		}
	}
}
