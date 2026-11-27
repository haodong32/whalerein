// Package routeproof renders the canonical Host route attachment proof shared
// by Host, Access and Central. The bytes are a wire contract: any difference
// invalidates the signature.
package routeproof

import (
	"crypto/ed25519"
	"encoding/base64"
	"fmt"
	"time"
)

// Canonical renders one attach proof. Field order and separators are fixed.
func Canonical(service, environment, hostID, nodeID, incarnation, challenge string, generation uint64, noiseKey []byte, signedAt time.Time) string {
	return fmt.Sprintf("whalerein/v1/host-route|attach|%s|%s|%s|%s|%s|%s|%d|%s|%d",
		service, environment, hostID, nodeID, incarnation, challenge, generation,
		base64.RawURLEncoding.EncodeToString(noiseKey), signedAt.UnixMilli())
}

// Verify checks one Host signature over the exact tuple.
func Verify(signingKey []byte, service, environment, hostID, nodeID, incarnation, challenge string, generation uint64, noiseKey []byte, signedAt time.Time, signature []byte) bool {
	if len(signingKey) != ed25519.PublicKeySize || len(signature) != ed25519.SignatureSize || generation == 0 {
		return false
	}
	return ed25519.Verify(ed25519.PublicKey(signingKey),
		[]byte(Canonical(service, environment, hostID, nodeID, incarnation, challenge, generation, noiseKey, signedAt)), signature)
}
