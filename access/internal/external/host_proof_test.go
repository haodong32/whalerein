package external

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"encoding/json"
	"errors"
	"testing"
	"time"
)

func TestHostOwnershipProofInputIsTheDocumentedCanonicalText(t *testing.T) {
	got := HostOwnershipProofInput("enrollment_status", "host-1", "op-1", 1_700_000_000_000, "bm9uY2U")
	want := "whalerein/v1/host-proof-v1\n|enrollment_status\n|host-1\n|op-1\n|1700000000000\n|bm9uY2U"
	if got != want {
		t.Fatalf("canonical input = %q, want %q", got, want)
	}
}

func TestParseHostOwnershipProofRoundTripsTheHeaderDocument(t *testing.T) {
	proof := HostOwnershipProof{
		HostID:         "host-1",
		IssuedAtUnixMs: 1_700_000_000_000,
		Nonce:          "bm9uY2U",
		Signature:      base64.RawURLEncoding.EncodeToString(make([]byte, ed25519.SignatureSize)),
	}
	document, err := json.Marshal(proof)
	if err != nil {
		t.Fatal(err)
	}
	parsed, err := ParseHostOwnershipProof(base64.RawURLEncoding.EncodeToString(document))
	if err != nil {
		t.Fatalf("unexpected parse error: %v", err)
	}
	if parsed != proof {
		t.Fatalf("parsed proof = %+v, want %+v", parsed, proof)
	}
}

func TestParseHostOwnershipProofRejectsMalformedDocuments(t *testing.T) {
	validSignature := base64.RawURLEncoding.EncodeToString(make([]byte, ed25519.SignatureSize))
	for name, document := range map[string]string{
		"not base64url": "%%%",
		"not json":      base64.RawURLEncoding.EncodeToString([]byte("{")),
		"missing host":  base64.RawURLEncoding.EncodeToString([]byte(`{"issuedAt":1,"nonce":"bm9uY2U","signature":"` + validSignature + `"}`)),
		"bad nonce":     base64.RawURLEncoding.EncodeToString([]byte(`{"hostId":"h","issuedAt":1,"nonce":"%%%","signature":"` + validSignature + `"}`)),
		"short sig":     base64.RawURLEncoding.EncodeToString([]byte(`{"hostId":"h","issuedAt":1,"nonce":"bm9uY2U","signature":"YQ"}`)),
	} {
		t.Run(name, func(t *testing.T) {
			if _, err := ParseHostOwnershipProof(document); !errors.Is(err, ErrProofInvalid) {
				t.Fatalf("expected a proof refusal, got %v", err)
			}
		})
	}
}

func TestVerifyHostOwnershipProofChecksWindowSignatureAndPurpose(t *testing.T) {
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_700_000_000, 0).UTC()
	sign := func(purpose, hostID, subject string, at time.Time, nonce string) HostOwnershipProof {
		signature := ed25519.Sign(privateKey,
			[]byte(HostOwnershipProofInput(purpose, hostID, subject, at.UnixMilli(), nonce)))
		return HostOwnershipProof{
			HostID: hostID, IssuedAtUnixMs: at.UnixMilli(), Nonce: nonce,
			Signature: base64.RawURLEncoding.EncodeToString(signature),
		}
	}
	if err := VerifyHostOwnershipProof(publicKey,
		sign(PurposeEnrollmentStatus, "host-1", "op-1", now, "bm9uY2U"),
		PurposeEnrollmentStatus, "op-1", now); err != nil {
		t.Fatalf("a valid proof was refused: %v", err)
	}
	// The same signature cannot authorize another purpose or subject.
	if err := VerifyHostOwnershipProof(publicKey,
		sign(PurposeEnrollmentStatus, "host-1", "op-1", now, "bm9uY2U"),
		PurposeCredentialReplacement, "op-1", now); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a purpose refusal, got %v", err)
	}
	if err := VerifyHostOwnershipProof(publicKey,
		sign(PurposeEnrollmentStatus, "host-1", "op-1", now, "bm9uY2U"),
		PurposeEnrollmentStatus, "op-2", now); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a subject refusal, got %v", err)
	}
	// The issue window is five minutes on either side.
	if err := VerifyHostOwnershipProof(publicKey,
		sign(PurposeEnrollmentStatus, "host-1", "op-1", now.Add(-6*time.Minute), "bm9uY2U"),
		PurposeEnrollmentStatus, "op-1", now); !errors.Is(err, ErrProofExpired) {
		t.Fatalf("expected a stale refusal, got %v", err)
	}
	if err := VerifyHostOwnershipProof(publicKey,
		sign(PurposeEnrollmentStatus, "host-1", "op-1", now.Add(6*time.Minute), "bm9uY2U"),
		PurposeEnrollmentStatus, "op-1", now); !errors.Is(err, ErrProofExpired) {
		t.Fatalf("expected a future refusal, got %v", err)
	}
}

func TestVerifyHostOwnershipProofRequiresAHostIdentity(t *testing.T) {
	publicKey, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	now := time.Unix(1_700_000_000, 0).UTC()
	proof := HostOwnershipProof{IssuedAtUnixMs: now.UnixMilli(), Nonce: "bm9uY2U"}
	if err := VerifyHostOwnershipProof(publicKey, proof, PurposeEnrollmentStatus, "", now); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("expected a Host id refusal, got %v", err)
	}
}
