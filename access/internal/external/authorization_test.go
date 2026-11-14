package external

import (
	"crypto/ed25519"
	"crypto/rand"
	"encoding/base64"
	"errors"
	"strings"
	"testing"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

type issuer struct {
	key     ed25519.PrivateKey
	public  ed25519.PublicKey
	trusted VerificationKey
}

func newIssuer(t *testing.T, id string, notBefore, notAfter time.Time) issuer {
	t.Helper()
	publicKey, privateKey, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	return issuer{
		key:     privateKey,
		public:  publicKey,
		trusted: VerificationKey{ID: id, PublicKey: publicKey, NotBefore: notBefore, NotAfter: notAfter},
	}
}

func (i issuer) authorize(t *testing.T, auth *whalereinv1.ExternalAuthorization) *whalereinv1.ExternalAuthorization {
	t.Helper()
	if auth.KeyId == "" {
		auth.KeyId = i.trusted.ID
	}
	auth.Signature = ed25519.Sign(i.key, []byte(CanonicalInput(auth)))
	return auth
}

var (
	issued  = time.Unix(1_700_000_000, 0).UTC()
	expires = issued.Add(5 * time.Minute)
)

func baseAuth(issuerID string) *whalereinv1.ExternalAuthorization {
	return &whalereinv1.ExternalAuthorization{
		AuthVersion:     AuthVersion,
		AuthorizationId: "auth-1",
		Issuer:          issuerID,
		Audience:        "https://access.fixture.test",
		Purpose:         PurposeHostEnrollment,
		WorkspaceId:     "ws-fixture-1",
		RegionId:        "fixture-a",
		OperationId:     "op-1",
		Precondition:    "initial_enrollment",
		HostId:          "host-1",
		IssuedAtUnixMs:  issued.UnixMilli(),
		ExpiresAtUnixMs: expires.UnixMilli(),
		Nonce:           []byte("0123456789abcdef"),
	}
}

func trustFor(signer issuer) Trust {
	return Trust{
		Issuer:          "fixture://issuer/a",
		ServiceIdentity: "https://access.fixture.test",
		Region:          "fixture-a",
		Keys:            []VerificationKey{signer.trusted},
	}
}

func TestVerifyAcceptsAWellFormedAuthorization(t *testing.T) {
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	auth := signer.authorize(t, baseAuth("fixture://issuer/a"))
	if err := trustFor(signer).Verify(auth, PurposeHostEnrollment, issued); err != nil {
		t.Fatalf("expected the authorization to verify: %v", err)
	}
}

func TestVerifyRejectsATamperedField(t *testing.T) {
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	auth := signer.authorize(t, baseAuth("fixture://issuer/a"))
	auth.WorkspaceId = "ws-fixture-2"
	if err := trustFor(signer).Verify(auth, PurposeHostEnrollment, issued); !errors.Is(err, ErrAuthorizationSignature) {
		t.Fatalf("expected a signature refusal, got %v", err)
	}
}

func TestVerifyRejectsAnUntrustedIssuer(t *testing.T) {
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	auth := signer.authorize(t, baseAuth("fixture://issuer/unknown"))
	if err := trustFor(signer).Verify(auth, PurposeHostEnrollment, issued); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("expected an issuer refusal, got %v", err)
	}
}

func TestVerifyRejectsTheWrongServiceRegionAndPurpose(t *testing.T) {
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	trust := trustFor(signer)
	for name, mutate := range map[string]func(*whalereinv1.ExternalAuthorization){
		"audience": func(a *whalereinv1.ExternalAuthorization) { a.Audience = "https://other.fixture.test" },
		"region":   func(a *whalereinv1.ExternalAuthorization) { a.RegionId = "fixture-b" },
		"purpose":  func(a *whalereinv1.ExternalAuthorization) { a.Purpose = PurposeDevicePairing },
	} {
		t.Run(name, func(t *testing.T) {
			auth := baseAuth("fixture://issuer/a")
			mutate(auth)
			signer.authorize(t, auth)
			err := trust.Verify(auth, PurposeHostEnrollment, issued)
			if !errors.Is(err, ErrAuthorizationMismatch) {
				t.Fatalf("expected a mismatch refusal, got %v", err)
			}
		})
	}
}

func TestVerifyRejectsAnExpiredAuthorization(t *testing.T) {
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	auth := signer.authorize(t, baseAuth("fixture://issuer/a"))
	if err := trustFor(signer).Verify(auth, PurposeHostEnrollment, expires); !errors.Is(err, ErrAuthorizationExpired) {
		t.Fatalf("expected an expiry refusal, got %v", err)
	}
}

func TestVerifyRejectsAKeyOutsideItsWindow(t *testing.T) {
	// The key is only valid tomorrow, so an authorization issued today cannot
	// have come from it.
	signer := newIssuer(t, "issuer-1", issued.Add(24*time.Hour), issued.Add(48*time.Hour))
	auth := signer.authorize(t, baseAuth("fixture://issuer/a"))
	if err := trustFor(signer).Verify(auth, PurposeHostEnrollment, issued); !errors.Is(err, ErrKeyNotValid) {
		t.Fatalf("expected a key-window refusal, got %v", err)
	}
}

func TestVerifyRejectsAnUnknownKeyId(t *testing.T) {
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	auth := baseAuth("fixture://issuer/a")
	auth.KeyId = "issuer-9"
	auth.Signature = ed25519.Sign(signer.key, []byte(CanonicalInput(auth)))
	if err := trustFor(signer).Verify(auth, PurposeHostEnrollment, issued); !errors.Is(err, ErrAuthorizationSignature) {
		t.Fatalf("expected an unknown-key refusal, got %v", err)
	}
}

func TestVerifyRejectsMissingAuthorization(t *testing.T) {
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	if err := trustFor(signer).Verify(nil, PurposeHostEnrollment, issued); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("expected a mismatch refusal, got %v", err)
	}
}

func enrollmentFixture(t *testing.T) (issuer, *whalereinv1.ExternalAuthorization, *whalereinv1.HostEnrollmentRequest) {
	t.Helper()
	signer, auth, request, _ := enrollmentFixtureWithKey(t)
	return signer, auth, request
}

// enrollmentFixtureWithKey also returns the Host signing key so a test can
// re-sign a changed request that must still pass the proof check.
func enrollmentFixtureWithKey(t *testing.T) (issuer, *whalereinv1.ExternalAuthorization, *whalereinv1.HostEnrollmentRequest, ed25519.PrivateKey) {
	t.Helper()
	signer := newIssuer(t, "issuer-1", issued.Add(-time.Hour), issued.Add(time.Hour))
	signingPublic, signingPrivate, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	noisePublic := make([]byte, 32)
	if _, err := rand.Read(noisePublic); err != nil {
		t.Fatal(err)
	}

	auth := baseAuth("fixture://issuer/a")
	auth.HostPublicKeys = [][]byte{signingPublic, noisePublic}
	signer.authorize(t, auth)

	request := &whalereinv1.HostEnrollmentRequest{
		Authorization:        auth,
		OperationId:          "op-1",
		HostId:               "host-1",
		HostSigningPublicKey: signingPublic,
		HostNoisePublicKey:   noisePublic,
	}
	request.ProofSignature = ed25519.Sign(signingPrivate, []byte(ProofInput(auth, "op-1", signingPublic)))
	return signer, auth, request, signingPrivate
}

func TestHostProofAcceptsTheApprovedKey(t *testing.T) {
	_, auth, request := enrollmentFixture(t)
	if err := VerifyHostProof(auth, request); err != nil {
		t.Fatalf("expected the proof to verify: %v", err)
	}
}

func TestHostProofRejectsAChangedKey(t *testing.T) {
	_, auth, request := enrollmentFixture(t)
	other, _, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	request.HostSigningPublicKey = other
	if err := VerifyHostProof(auth, request); !errors.Is(err, ErrAuthorizationMismatch) {
		t.Fatalf("expected a key mismatch, got %v", err)
	}
}

func TestHostProofRejectsAProofFromAnotherKey(t *testing.T) {
	_, auth, request := enrollmentFixture(t)
	_, impostor, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	request.ProofSignature = ed25519.Sign(impostor, []byte(ProofInput(auth, "op-1", request.HostSigningPublicKey)))
	if err := VerifyHostProof(auth, request); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a proof refusal, got %v", err)
	}
}

func TestHostProofRejectsAReplayedProofForAnotherOperation(t *testing.T) {
	_, auth, request := enrollmentFixture(t)
	request.OperationId = "op-2"
	auth.OperationId = "op-2"
	// The signature still covers op-1, so the replay must be refused.
	if err := VerifyHostProof(auth, request); !errors.Is(err, ErrProofInvalid) {
		t.Fatalf("expected a proof refusal, got %v", err)
	}
}

func TestHostProofRejectsMismatchedIdentifierAndKeySizes(t *testing.T) {
	for name, mutate := range map[string]func(*whalereinv1.HostEnrollmentRequest){
		"host id":      func(r *whalereinv1.HostEnrollmentRequest) { r.HostId = "host-2" },
		"operation id": func(r *whalereinv1.HostEnrollmentRequest) { r.OperationId = "op-9" },
		"short signing key": func(r *whalereinv1.HostEnrollmentRequest) {
			r.HostSigningPublicKey = r.HostSigningPublicKey[:16]
		},
		"short noise key": func(r *whalereinv1.HostEnrollmentRequest) {
			r.HostNoisePublicKey = r.HostNoisePublicKey[:16]
		},
	} {
		t.Run(name, func(t *testing.T) {
			_, auth, request := enrollmentFixture(t)
			mutate(request)
			if err := VerifyHostProof(auth, request); err == nil {
				t.Fatal("expected a refusal")
			}
		})
	}
}

func TestDigestBindsBothKeys(t *testing.T) {
	_, _, request := enrollmentFixture(t)
	first := Digest(request)
	original := append([]byte(nil), request.HostNoisePublicKey...)
	request.HostNoisePublicKey[0] ^= 0xff
	if string(Digest(request)) == string(first) {
		t.Fatal("changing the Noise key must change the digest")
	}
	request.HostNoisePublicKey = original
	if string(Digest(request)) != string(first) {
		t.Fatal("the digest must be stable for the same payload")
	}
}

func TestCanonicalInputBindsEveryScopeField(t *testing.T) {
	base := baseAuth("fixture://issuer/a")
	reference := CanonicalInput(base)
	for name, mutate := range map[string]func(*whalereinv1.ExternalAuthorization){
		"workspace":    func(a *whalereinv1.ExternalAuthorization) { a.WorkspaceId = "ws-2" },
		"operation":    func(a *whalereinv1.ExternalAuthorization) { a.OperationId = "op-2" },
		"host":         func(a *whalereinv1.ExternalAuthorization) { a.HostId = "host-2" },
		"nonce":        func(a *whalereinv1.ExternalAuthorization) { a.Nonce = []byte("another-nonce") },
		"precondition": func(a *whalereinv1.ExternalAuthorization) { a.Precondition = "" },
		"expected":     func(a *whalereinv1.ExternalAuthorization) { a.ExpectedRevision = "4" },
	} {
		t.Run(name, func(t *testing.T) {
			auth := baseAuth("fixture://issuer/a")
			mutate(auth)
			if CanonicalInput(auth) == reference {
				t.Fatalf("changing the %s must change the signing input", name)
			}
		})
	}
}

func TestHashKeysIsOrderIndependentAndLengthPrefixed(t *testing.T) {
	forward := HashKeys([][]byte{[]byte("a"), []byte("b"), []byte("c")})
	reversed := HashKeys([][]byte{[]byte("c"), []byte("b"), []byte("a")})
	if forward != reversed {
		t.Fatal("key hashing must not depend on order")
	}
	if HashKeys([][]byte{[]byte("ab"), []byte("c")}) == HashKeys([][]byte{[]byte("a"), []byte("bc")}) {
		t.Fatal("length prefixes must separate different key sets")
	}
	if HashKeys(nil) != HashKeys([][]byte{}) {
		t.Fatal("nil and empty key sets must hash equally")
	}
}

func TestProofInputIsStableAndBase64URL(t *testing.T) {
	auth := baseAuth("fixture://issuer/a")
	input := ProofInput(auth, "op-1", []byte("signing-key"))
	if input != ProofInput(auth, "op-1", []byte("signing-key")) {
		t.Fatal("the proof input must be deterministic")
	}
	if input == ProofInput(auth, "op-2", []byte("signing-key")) {
		t.Fatal("the operation id must be bound into the proof input")
	}
	if !strings.HasPrefix(input, ProofPrefix+"|"+auth.AuthorizationId) {
		t.Fatalf("the proof input must start with the domain separator: %q", input)
	}
	encodedNonce := base64.RawURLEncoding.EncodeToString(auth.Nonce)
	if !strings.Contains(input, "|"+encodedNonce+"|") {
		t.Fatalf("the nonce must appear as raw URL base64: %q", input)
	}
	for _, character := range encodedNonce {
		if character == '+' || character == '/' || character == '=' {
			t.Fatalf("nonce encoding must avoid padding and the standard alphabet: %q", encodedNonce)
		}
	}
}

// enrollmentKeyPair returns one X25519-sized Noise key for a replacement case.
func enrollmentKeyPair(t *testing.T) ([]byte, ed25519.PrivateKey, error) {
	t.Helper()
	noise := make([]byte, 32)
	if _, err := rand.Read(noise); err != nil {
		t.Fatal(err)
	}
	_, private, err := ed25519.GenerateKey(rand.Reader)
	return noise, private, err
}

// signProof signs the enrollment proof input for a changed request.
func signProof(t *testing.T, original *whalereinv1.HostEnrollmentRequest, auth *whalereinv1.ExternalAuthorization, changed *whalereinv1.HostEnrollmentRequest) []byte {
	t.Helper()
	// The fixture keeps the signing private key out of the request, so the
	// test signs with a freshly generated key and asserts the refusal instead.
	_, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	return ed25519.Sign(private, []byte(ProofInput(auth, changed.GetOperationId(), changed.GetHostSigningPublicKey())))
}

func TestVerifyTreatsAnOmittedBoundAsOpen(t *testing.T) {
	public, private, err := ed25519.GenerateKey(rand.Reader)
	if err != nil {
		t.Fatal(err)
	}
	signer := ed25519.PrivateKey(private)
	auth := &whalereinv1.ExternalAuthorization{
		AuthVersion:     AuthVersion,
		AuthorizationId: "auth-window",
		Purpose:         PurposeHostEnrollment,
		Issuer:          "fixture://issuer/a",
		Audience:        "https://access.fixture.test",
		RegionId:        "fixture-a",
		HostId:          "host-1",
		IssuedAtUnixMs:  time.Now().UnixMilli(),
		ExpiresAtUnixMs: time.Now().Add(time.Minute).UnixMilli(),
		Nonce:           []byte("0123456789abcdef"),
		KeyId:           "issuer-1",
	}
	auth.Signature = ed25519.Sign(signer, []byte(CanonicalInput(auth)))

	base := Trust{
		Issuer:          "fixture://issuer/a",
		ServiceIdentity: "https://access.fixture.test",
		Region:          "fixture-a",
	}

	open := base
	open.Keys = []VerificationKey{{ID: "issuer-1", PublicKey: public}}
	if err := open.Verify(auth, PurposeHostEnrollment, time.Now()); err != nil {
		t.Fatalf("an open window must be accepted: %v", err)
	}

	future := base
	future.Keys = []VerificationKey{{ID: "issuer-1", PublicKey: public,
		NotBefore: time.Now().Add(time.Hour)}}
	if err := future.Verify(auth, PurposeHostEnrollment, time.Now()); !errors.Is(err, ErrKeyNotValid) {
		t.Fatalf("a future not_before must refuse: %v", err)
	}

	expired := base
	expired.Keys = []VerificationKey{{ID: "issuer-1", PublicKey: public,
		NotAfter: time.Now().Add(-time.Hour)}}
	if err := expired.Verify(auth, PurposeHostEnrollment, time.Now()); !errors.Is(err, ErrKeyNotValid) {
		t.Fatalf("a past not_after must refuse: %v", err)
	}

	bounded := base
	bounded.Keys = []VerificationKey{{ID: "issuer-1", PublicKey: public,
		NotBefore: time.Now().Add(-time.Hour), NotAfter: time.Now().Add(time.Hour)}}
	if err := bounded.Verify(auth, PurposeHostEnrollment, time.Now()); err != nil {
		t.Fatalf("a valid window must be accepted: %v", err)
	}
}
