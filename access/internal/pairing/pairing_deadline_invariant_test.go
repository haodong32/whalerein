package pairing

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/proto"
)

func TestPairingHelloCannotPersistAfterHostRoundTripExpires(t *testing.T) {
	for _, tc := range []struct {
		name        string
		expireToken bool
		advance     time.Duration
		want        error
	}{
		{"invitation expires", true, 31 * time.Second, ErrTokenExpired},
		{"intent deadline expires", false, 61 * time.Second, ErrClaimRequired},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newFixture(t)
			_, token := f.invite(t)
			if tc.expireToken {
				hash := sha256.Sum256([]byte(token))
				current := f.store.tokens[hashKey(hash[:])]
				current.ExpiresAt = f.now.Add(30 * time.Second)
				f.store.tokens[hashKey(hash[:])] = current
			}
			f.transport.awaitFn = func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
				f.now = f.now.Add(tc.advance)
				return defaultPairingAnswers(request)
			}
			_, err := f.service.Claim(context.Background(), helloRequest(token, "hello-deadline", "d1"))
			if !errors.Is(err, tc.want) {
				t.Fatalf("unexpected deadline result: %v", err)
			}
			if len(f.store.operations) != 0 || len(f.store.grants) != 0 {
				t.Fatal("expired HELLO persisted pairing intent")
			}
		})
	}
}

func TestPairingStorageDeadlineErrorsRemainDistinctFromRevocation(t *testing.T) {
	for _, tc := range []struct {
		name             string
		duringHello      bool
		storageErr, want error
	}{
		{"HELLO invitation", true, store.ErrPairingInvitationExpired, ErrTokenExpired},
		{"HELLO intent", true, store.ErrPairingIntentExpired, ErrClaimRequired},
		{"FINISH invitation", false, store.ErrPairingInvitationExpired, ErrTokenExpired},
		{"FINISH intent", false, store.ErrPairingIntentExpired, ErrClaimRequired},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newFixture(t)
			_, token := f.invite(t)
			ctx := context.Background()
			if tc.duringHello {
				f.store.saveErr = tc.storageErr
				_, err := f.service.Claim(ctx, helloRequest(token, "deadline-mapping", "d1"))
				if !errors.Is(err, tc.want) {
					t.Fatal("HELLO deadline reported wrong outcome")
				}
			} else {
				if _, err := f.service.Claim(ctx, helloRequest(token, "deadline-mapping", "d1")); err != nil {
					t.Fatal(err)
				}
				f.store.deviceErr = tc.storageErr
				_, err := f.service.Claim(ctx, finishRequest(token, "deadline-mapping", "d1"))
				if !errors.Is(err, tc.want) {
					t.Fatal("FINISH deadline reported wrong outcome")
				}
			}
			if len(f.store.used) != 0 || len(f.store.grants) != 0 || len(f.store.notices) != 0 {
				t.Fatal("deadline failure changed durable pairing state")
			}
		})
	}
}

func TestPairingFinishCannotCommitAfterHostRoundTripExpires(t *testing.T) {
	for _, tc := range []struct {
		name                         string
		phase                        whalereinv1.PairingPhase
		expireToken, extendOperation bool
		advance                      time.Duration
		want                         error
	}{
		{"invitation expires during FINISH", whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, true, false, 31 * time.Second, ErrTokenExpired},
		{"operation expires during FINISH", whalereinv1.PairingPhase_PAIRING_PHASE_FINISH, false, false, 61 * time.Second, ErrClaimRequired},
		{"operation expires during AUTHORIZE", whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE, false, false, 61 * time.Second, ErrClaimRequired},
		{"outbox deadline expires during AUTHORIZE", whalereinv1.PairingPhase_PAIRING_PHASE_AUTHORIZE, false, true, 61 * time.Second, ErrClaimRequired},
	} {
		t.Run(tc.name, func(t *testing.T) {
			f := newFixture(t)
			_, token := f.invite(t)
			ctx := context.Background()
			if _, err := f.service.Claim(ctx, helloRequest(token, "deadline", "d1")); err != nil {
				t.Fatal(err)
			}
			hash := sha256.Sum256([]byte(token))
			if tc.expireToken {
				current := f.store.tokens[hashKey(hash[:])]
				current.ExpiresAt = f.now.Add(30 * time.Second)
				f.store.tokens[hashKey(hash[:])] = current
			}
			if tc.extendOperation {
				operation := f.store.operations["deadline"]
				operation.ExpiresAt = f.now.Add(5 * time.Minute)
				f.store.operations["deadline"] = operation
			}
			advanced := false
			f.transport.awaitFn = func(request *whalereinv1.HostRequest) (*whalereinv1.HostResult, error) {
				var claim whalereinv1.PairingClaim
				if err := proto.Unmarshal(request.Body, &claim); err != nil {
					return nil, err
				}
				if !advanced && claim.Phase == tc.phase {
					f.now = f.now.Add(tc.advance)
					advanced = true
				}
				return defaultPairingAnswers(request)
			}
			_, err := f.service.Claim(ctx, finishRequest(token, "deadline", "d1"))
			if !advanced || !errors.Is(err, tc.want) {
				t.Fatalf("unexpected deadline result: %v", err)
			}
			if len(f.store.used) != 0 || len(f.store.grants) != 0 || len(f.store.notices) != 0 || f.store.confirmCalls != 0 {
				t.Fatal("expired pairing changed durable state")
			}
		})
	}
}

func TestPairingRechecksInvitationDeadlineAfterTokenLock(t *testing.T) {
	f := newFixture(t)
	_, token := f.invite(t)
	hash := sha256.Sum256([]byte(token))
	hashHex := hex.EncodeToString(hash[:])
	unlock, err := f.service.lockToken(context.Background(), hashHex)
	if err != nil {
		t.Fatal(err)
	}
	done := make(chan error, 1)
	go func() {
		_, err := f.service.Claim(context.Background(), helloRequest(token, "held-expiry", "d1"))
		done <- err
	}()
	deadline := time.Now().Add(3 * time.Second)
	for {
		f.service.locksMu.Lock()
		entry := f.service.locks[hashHex]
		waiting := entry != nil && entry.refs == 2
		f.service.locksMu.Unlock()
		if waiting {
			break
		}
		if time.Now().After(deadline) {
			unlock()
			t.Fatal("claim did not wait for token lock")
		}
		time.Sleep(time.Millisecond)
	}
	f.now = f.now.Add(11 * time.Minute)
	unlock()
	if err := <-done; !errors.Is(err, ErrTokenExpired) {
		t.Fatalf("expired invitation was not rejected: %v", err)
	}
	if len(f.transport.requests) != 0 || len(f.store.operations) != 0 {
		t.Fatal("expired invitation reached the Host or persisted an intent")
	}
}
