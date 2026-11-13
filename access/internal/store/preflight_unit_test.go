package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5/pgxpool"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestStoreRejectsInvalidInputsBeforeDatabaseAccess(t *testing.T) {
	ctx := context.Background()
	// A nil pool makes any accidental database access fail the test.
	s := &Store{}
	for _, tc := range []struct {
		name    string
		request Request
	}{
		{"unsupported receiver capability", Request{ClientTransportVersion: 3}},
		{"unspecified status", Request{Status: whalereinv1.OnlineStatus_online_status_unspecified}},
		{"invalid forward state", Request{Status: whalereinv1.OnlineStatus_received, ForwardState: ForwardState("invalid")}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			if _, err := s.CreateRequest(ctx, tc.request); err == nil {
				t.Fatal("invalid request was accepted")
			}
		})
	}
	if accepted, err := s.ApplyHostResult(ctx, HostResultUpdate{}); accepted || err == nil {
		t.Fatal("invalid result status was accepted")
	}
	if _, err := s.EnsureHost(ctx, ""); err == nil {
		t.Fatal("missing host was accepted")
	}
	for _, tc := range []struct {
		host string
		key  []byte
	}{{"", make([]byte, 32)}, {"host", make([]byte, 31)}} {
		if err := s.SetHostAuthorizationKey(ctx, tc.host, tc.key); !errors.Is(err, ErrNotFound) {
			t.Fatal("invalid authorization key was accepted")
		}
	}
	if err := s.MarkRevocationsReplayed(ctx, "host", nil, time.Time{}, time.Time{}, nil); err != nil {
		t.Fatalf("empty acknowledgement: %v", err)
	}
	for _, limit := range []int{0, -1} {
		rows, err := s.ClaimDueRequests(ctx, time.Time{}, limit, time.Second)
		if err != nil || len(rows) != 0 {
			t.Fatal("nonpositive lease limit must not access the database")
		}
	}
	if err := s.Ping(ctx); err == nil {
		t.Fatal("missing pool was accepted")
	}
	s.Close()
}

func TestInvalidDatabaseURLDoesNotExposeCredentialsInError(t *testing.T) {
	_, err := Open(context.Background(), "postgres://fixture-user:fixture-password@%zz/fixture")
	if err == nil {
		t.Fatal("invalid database URL was accepted")
	}
	if user, password := strings.Contains(err.Error(), "fixture-user"), strings.Contains(err.Error(), "fixture-password"); user || password {
		t.Fatalf("database error exposed credential fields: u:%t p:%t", user, password)
	}
}

func TestStoreOwnsAndClosesItsSuppliedLazyPool(t *testing.T) {
	cfg, err := pgxpool.ParseConfig("postgres://localhost:1/fixture?sslmode=disable")
	if err != nil {
		t.Fatal(err)
	}
	pool, err := pgxpool.NewWithConfig(context.Background(), cfg)
	if err != nil {
		t.Fatal(err)
	}
	s := NewWithPool(pool)
	if s.Pool() != pool {
		t.Fatal("store lost the supplied pool")
	}
	s.Close()
	if _, err := pool.Acquire(context.Background()); err == nil {
		t.Fatal("closed store left its pool usable")
	}
}

func TestOpenCancelledContextDoesNotLeaveUsablePool(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	value, err := Open(ctx, "postgres://localhost:1/fixture?sslmode=disable")
	if value != nil || !errors.Is(err, context.Canceled) {
		t.Fatalf("cancelled open returned store or wrong error: %v", err)
	}
}
