package relay

import (
	"context"
	"log/slog"
	"time"

	"github.com/whalerein/whalerein/access/internal/payload"
)

// runPayloadStorage drains expired temporary ciphertext and periodically
// repeats bounded reconciliation. Failed cleanup stays charged.
func RunPayloadStorage(ctx context.Context, service *payload.Service, stager *Stager, interval time.Duration, logger *slog.Logger) {
	if interval <= 0 {
		interval = 30 * time.Second
	}
	tick := 0
	run := func() {
		if _, err := service.Cleanup(ctx); err != nil && ctx.Err() == nil {
			logger.Warn("payload cleanup failed", "error", err.Error())
		}
		// A revoked grant or ended session closes admission immediately; this
		// bounded sweep cancels the affected objects best-effort. Release was
		// already fenced by the directory's grant revision check.
		if cancelled, err := stager.SweepRevoked(ctx); err != nil && ctx.Err() == nil {
			logger.Warn("payload revocation sweep failed", "error", err.Error())
		} else if cancelled > 0 {
			logger.Info("revoked ciphertext scopes cancelled", "count", cancelled)
		}
		tick++
		if tick%10 != 0 {
			return
		}
		if err := service.Recover(ctx); err != nil && ctx.Err() == nil {
			logger.Warn("payload storage reconciliation failed", "error", err.Error())
		}
	}
	run()
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			run()
		}
	}
}
