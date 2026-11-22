package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func requestFixtureRow(digest []byte) pgx.Row {
	at := time.Unix(123, 0).UTC()
	return syntheticRow{fill: func(d ...any) {
		*d[0].(*string) = "request"
		*d[1].(*string) = "host"
		*d[2].(*string) = "device"
		*d[3].(*string) = "command"
		*d[4].(*[]byte) = digest
		*d[5].(*int32) = int32(whalereinv1.RequestKind_REQUEST_KIND_COMMAND)
		*d[6].(*uint32) = 1
		*d[7].(*string) = "received"
		*d[8].(*[]byte) = []byte{}
		*d[9].(*[]byte) = []byte{}
		*d[10].(*int32) = 0
		*d[11].(*string) = ""
		*d[12].(*string) = "channel"
		*d[13].(*string) = "trace"
		*d[14].(*time.Time) = at
		*d[15].(*string) = "pending"
		*d[16].(*time.Time) = at
		*d[17].(*int) = 0
		*d[18].(*bool) = false
		*d[19].(*time.Time) = at
		*d[20].(*time.Time) = at
		*d[21].(*uint32) = 2
		*d[22].(**time.Time) = nil
		*d[23].(*string) = "live"
		*d[24].(*bool) = false
		*d[25].(**time.Time) = nil
		*d[26].(*int) = 0
		grant, revocation, deviceRevocation, resultLimit := int64(1), int64(1), int64(1), int64(16<<20)
		*d[27].(**int64) = &grant
		*d[28].(**int64) = &revocation
		*d[29].(**int64) = &deviceRevocation
		*d[30].(**int64) = &resultLimit
		*d[31].(*int32) = 3
		*d[32].(*[]byte) = []byte{}
		*d[33].(*[]byte) = []byte{}
		*d[34].(*bool) = false
		*d[35].(*int) = 0
		*d[36].(**time.Time) = nil
		*d[37].(**time.Time) = nil
		*d[38].(*string) = ""
		*d[39].(**time.Time) = nil
	}}
}
func TestCreateRequestDeduplicatesOnlyWithinHostDeviceCommandAndDigest(t *testing.T) {
	marker := errors.New("synthetic query failure")
	for _, tc := range []struct {
		name, mode        string
		wantErr           error
		created, mismatch bool
		queries           int
	}{
		{"new command", "created", nil, true, false, 1},
		{"same command and digest", "same", nil, false, false, 2},
		{"changed digest", "changed", nil, false, true, 2},
		{"duplicate lookup unavailable", "lookup-failure", marker, false, false, 2},
		{"insertion fails", "failure", marker, false, false, 1},
		{"unique request identity collision", "unique", ErrConflict, false, false, 1},
	} {
		t.Run(tc.name, func(t *testing.T) {
			calls := 0
			query := func(_ context.Context, sql string, args ...any) pgx.Row {
				calls++
				if calls == 1 {
					if !strings.Contains(sql, "ON CONFLICT (host_id, device_id, command_id)") || len(args) != 40 || args[1] != "host" || args[2] != "device" || args[3] != "command" {
						t.Fatal("insert did not preserve the dedupe scope")
					}
					switch tc.mode {
					case "same", "changed", "lookup-failure":
						return syntheticRow{err: pgx.ErrNoRows}
					case "failure":
						return syntheticRow{err: marker}
					case "unique":
						return syntheticRow{err: &pgconn.PgError{Code: "23505"}}
					default:
						return requestFixtureRow([]byte{1})
					}
				}
				if len(args) != 3 || args[0] != "host" || args[1] != "device" || args[2] != "command" {
					t.Fatal("lookup crossed the dedupe scope")
				}
				if tc.mode == "lookup-failure" {
					return syntheticRow{err: marker}
				}
				if tc.mode == "changed" {
					return requestFixtureRow([]byte{2})
				}
				return requestFixtureRow([]byte{1})
			}
			result, err := createRequest(context.Background(), query, Request{RequestID: "request", HostID: "host", DeviceID: "device", CommandID: "command", PayloadDigest: []byte{1}, Kind: whalereinv1.RequestKind_REQUEST_KIND_COMMAND, Status: whalereinv1.OnlineStatus_received, ClientTransportVersion: 2, Stamp: &GrantStamp{GrantGeneration: 1, RevocationGeneration: 1, DeviceRevocationGeneration: 1, ResultLimitBytes: 16 << 20}})
			if !errors.Is(err, tc.wantErr) || result.Created != tc.created || result.DigestMismatch != tc.mismatch || calls != tc.queries {
				t.Fatalf("err=%v created=%v mismatch=%v queries=%d", err, result.Created, result.DigestMismatch, calls)
			}
		})
	}
}
func TestCreateReadOnlyRequestHasNoCommandDeduplication(t *testing.T) {
	marker := errors.New("synthetic query failure")
	for _, tc := range []struct {
		name          string
		scanErr, want error
	}{
		{"insert", nil, nil},
		{"duplicate request identity", &pgconn.PgError{Code: "23505"}, ErrConflict},
		{"storage failure", marker, marker},
	} {
		t.Run(tc.name, func(t *testing.T) {
			calls := 0
			query := func(_ context.Context, sql string, args ...any) pgx.Row {
				calls++
				if strings.Contains(sql, "ON CONFLICT") || len(args) != 40 || args[3] != "" {
					t.Fatal("read-only request was treated as a control")
				}
				if tc.scanErr != nil {
					return syntheticRow{err: tc.scanErr}
				}
				return requestFixtureRow(nil)
			}
			result, err := createRequest(context.Background(), query, Request{Status: whalereinv1.OnlineStatus_received})
			if !errors.Is(err, tc.want) || result.Created != (tc.want == nil) || calls != 1 {
				t.Fatalf("err=%v created=%v calls=%d", err, result.Created, calls)
			}
		})
	}
}
