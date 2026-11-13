package store

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

// proofEvidenceTx scripts exactly the durable COMMIT evidence reads used by the
// retention and confirmation paths. It never forwards SQL to a database.
type proofEvidenceTx struct {
	pgx.Tx
	t               *testing.T
	requestExists   bool
	key             []byte
	protocol        uint32
	active          bool
	bindingVerified bool
	bindingBody     []byte
	bindingErr      error
	cleaned         bool
	cleanedErr      error
	fingerprint     string
	updateRows      int64
	updateErr       error
	steps           []string
}

func (tx *proofEvidenceTx) QueryRow(_ context.Context, sql string, args ...any) pgx.Row {
	switch {
	case strings.Contains(sql, "bodies_cleared IS TRUE"):
		tx.steps = append(tx.steps, "cleaned")
		return syntheticRow{err: tx.cleanedErr, fill: func(dest ...any) { *dest[0].(*bool) = tx.cleaned }}
	case strings.Contains(sql, "SELECT EXISTS(SELECT 1 FROM requests"):
		tx.steps = append(tx.steps, "request")
		if len(args) != 5 {
			tx.t.Fatal("request existence must be scoped by identity and deadline")
		}
		return syntheticRow{fill: func(dest ...any) { *dest[0].(*bool) = tx.requestExists }}
	case strings.Contains(sql, "SELECT o.device_public_key"):
		tx.steps = append(tx.steps, "outcome")
		return syntheticRow{fill: func(dest ...any) {
			*dest[0].(*[]byte) = tx.key
			*dest[1].(*uint32) = tx.protocol
			*dest[2].(*bool) = tx.active
			*dest[3].(*bool) = tx.bindingVerified
		}}
	case strings.Contains(sql, "SELECT public_key_fingerprint"):
		tx.steps = append(tx.steps, "fingerprint")
		return syntheticRow{fill: func(dest ...any) { *dest[0].(*string) = tx.fingerprint }}
	case strings.Contains(sql, "SELECT r.request_body,o.grant_generation"):
		tx.steps = append(tx.steps, "binding")
		if tx.bindingErr != nil {
			return syntheticRow{err: tx.bindingErr}
		}
		created := time.Unix(100, 0).UTC()
		expires := created.Add(time.Minute)
		return syntheticRow{fill: func(dest ...any) {
			*dest[0].(*[]byte) = tx.bindingBody
			*dest[1].(*uint64) = 1
			*dest[2].(*string) = tx.fingerprint
			*dest[3].(*uint32) = tx.protocol
			*dest[4].(*time.Time) = created
			*dest[5].(*time.Time) = expires
		}}
	default:
		tx.t.Fatalf("unexpected evidence query: %s", sql)
		return nil
	}
}

func (tx *proofEvidenceTx) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	if strings.Contains(sql, "pg_advisory_xact_lock") {
		tx.steps = append(tx.steps, "lock")
		return pgconn.NewCommandTag("SELECT 1"), nil
	}
	tx.steps = append(tx.steps, "update")
	if tx.updateErr != nil {
		return pgconn.CommandTag{}, tx.updateErr
	}
	return pgconn.NewCommandTag("UPDATE " + itoa(tx.updateRows)), nil
}

func itoa(value int64) string {
	if value == 0 {
		return "0"
	}
	return "1"
}

func sha256Sum(value []byte) [32]byte { return sha256.Sum256(value) }

// Missing durable identity is not proof, an absent outbox row is not proof, and
// a storage failure must stay an error rather than silently downgrade.
func TestPersistedCommitProofRequiresDurableOutboxIdentity(t *testing.T) {
	key := make([]byte, 32)
	key[0] = 1

	t.Run("missing identity", func(t *testing.T) {
		tx := &proofEvidenceTx{t: t, key: key, protocol: 1}
		accepted, err := verifyPersistedCommitTx(context.Background(), tx, "", "host", "device", "request", key, 1)
		if accepted || err != nil {
			t.Fatalf("missing identity accepted=%v err=%v", accepted, err)
		}
		for _, step := range tx.steps {
			if step == "binding" {
				t.Fatal("missing identity reached the outbox query")
			}
		}
	})

	t.Run("outbox row absent", func(t *testing.T) {
		tx := &proofEvidenceTx{t: t, key: key, protocol: 1, bindingErr: pgx.ErrNoRows}
		accepted, err := verifyPersistedCommitTx(context.Background(), tx, "claim", "host", "device", "request", key, 1)
		if accepted || err != nil {
			t.Fatalf("absent outbox accepted=%v err=%v", accepted, err)
		}
	})

	t.Run("outbox read failure", func(t *testing.T) {
		marker := errors.New("synthetic evidence failure")
		tx := &proofEvidenceTx{t: t, key: key, protocol: 1, bindingErr: marker}
		if accepted, err := verifyPersistedCommitTx(context.Background(), tx, "claim", "host", "device", "request", key, 1); accepted || !errors.Is(err, marker) {
			t.Fatalf("storage failure accepted=%v err=%v", accepted, err)
		}
	})
}

// Retention may deliberately clear a previously verified outbox body. That
// must remain confirmable, while a contradictory body or a never-verified
// binding must never be treated as proof.
func TestConfirmPairingHostRequiresRetainedCommitProof(t *testing.T) {
	at := time.Unix(100, 0).UTC()
	key := make([]byte, 32)
	key[0] = 1
	pin := sha256Sum(key)
	fingerprint := hex.EncodeToString(pin[:])
	bindingFailure := errors.New("synthetic binding failure")
	cleanedFailure := errors.New("synthetic cleaned failure")

	for _, tc := range []struct {
		name            string
		identity        string
		bindingVerified bool
		bindingBody     []byte
		bindingErr      error
		cleaned         bool
		cleanedErr      error
		updateRows      int64
		expectUpdate    bool
		want            bool
		wantErr         error
	}{
		{"missing identity", "", true, nil, nil, true, nil, 1, false, false, ErrConflict},
		{"binding read failure", "claim", false, nil, bindingFailure, true, nil, 1, false, false, bindingFailure},
		{"unproven never verified", "claim", false, []byte{}, nil, true, nil, 1, false, false, ErrConflict},
		{"retention cleared retained proof", "claim", true, []byte{}, nil, true, nil, 1, true, true, nil},
		{"contradictory body", "claim", true, []byte{0xff}, nil, false, nil, 1, false, false, ErrConflict},
		{"cleared evidence read failure", "claim", true, []byte{}, nil, false, cleanedFailure, 1, false, false, cleanedFailure},
		{"confirmation write conflict", "claim", true, []byte{}, nil, true, nil, 0, true, false, ErrConflict},
	} {
		t.Run(tc.name, func(t *testing.T) {
			tx := &proofEvidenceTx{
				t: t, requestExists: true, key: key, protocol: 1, active: true,
				bindingVerified: tc.bindingVerified, bindingBody: tc.bindingBody,
				bindingErr: tc.bindingErr, cleaned: tc.cleaned, cleanedErr: tc.cleanedErr,
				fingerprint: fingerprint, updateRows: tc.updateRows,
			}
			confirmed, err := confirmPairingHostTx(context.Background(), tx, tc.identity, "host", "device", "request", at, true)
			if tc.wantErr != nil {
				if confirmed || !errors.Is(err, tc.wantErr) {
					t.Fatalf("confirmed=%v err=%v", confirmed, err)
				}
				for _, step := range tx.steps {
					if step == "update" && !tc.expectUpdate {
						t.Fatal("unproven evidence was written as confirmation")
					}
				}
				return
			}
			if confirmed != tc.want || err != nil {
				t.Fatalf("confirmed=%v err=%v steps=%v", confirmed, err, tx.steps)
			}
		})
	}
}
