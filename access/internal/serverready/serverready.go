// Package serverready owns the small readiness and listener-timeout decisions
// that the Access process makes before it starts serving. Keeping them here
// makes each branch testable without a database, broker or listener.
package serverready

import (
	"context"
	"errors"
	"time"

	"github.com/whalerein/whalerein/access/internal/diagnostics"
)

// Session reports whether the broker session and its subscriptions are ready.
type Session interface{ Ready() bool }

// Presence reports whether broker presence has been reconciled.
type Presence interface{ Available() bool }

// Storage reports whether temporary ciphertext storage can accept work.
type Storage interface {
	Enabled() bool
	Ready(context.Context) error
}

// AddChecks registers the broker readiness check and, when the operator
// enabled it, the storage check. Database readiness is registered separately
// because it owns the write probe.
func AddChecks(registry *diagnostics.Readiness, session Session, presence Presence, storage Storage) {
	registry.Add("broker", func(context.Context) error {
		if !session.Ready() {
			return errors.New("mqtt session is not ready")
		}
		if !presence.Available() {
			return errors.New("broker presence is not reconciled")
		}
		return nil
	})
	if storage != nil && storage.Enabled() {
		registry.Add("storage", storage.Ready)
	}
}

// HTTPTimeouts keeps the listener bounds at least as large as the configured
// payload transfer timeout plus a small grace when storage is enabled, without
// changing the behavior of any other route.
func HTTPTimeouts(read, write time.Duration, storageEnabled bool, upload, storageRead time.Duration) (time.Duration, time.Duration) {
	if !storageEnabled {
		return read, write
	}
	if bounded := upload + 5*time.Second; bounded > read {
		read = bounded
	}
	if bounded := storageRead + 5*time.Second; bounded > write {
		write = bounded
	}
	return read, write
}
