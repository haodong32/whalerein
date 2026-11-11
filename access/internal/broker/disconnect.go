package broker

import (
	"context"
	"errors"
	"fmt"
	"os/exec"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
)

// CommandRunner runs one external command. It is replaceable in tests.
type CommandRunner func(ctx context.Context, name string, args ...string) ([]byte, error)

// Disconnecter actively closes one broker client connection.
type Disconnecter interface {
	Disconnect(ctx context.Context, clientID string) error
}

type ctlDisconnecter struct {
	path  string
	run   CommandRunner
	now   func() time.Time
	limit time.Duration
}

// NewDisconnecter builds the configured EMQX disconnect mechanism. REST is
// used when configured; otherwise Access invokes `emqx ctl clients kick`,
// which is the local/self-hosted default.
func NewDisconnecter(cfg config.EMQXConfig, rest *RESTClient, run CommandRunner) (Disconnecter, error) {
	switch cfg.DisconnectMode {
	case "", "ctl":
		if strings.TrimSpace(cfg.CTLPath) == "" {
			return nil, errors.New("emqx.ctl_path is required for ctl disconnect mode")
		}
		if run == nil {
			run = func(ctx context.Context, name string, args ...string) ([]byte, error) {
				return exec.CommandContext(ctx, name, args...).CombinedOutput()
			}
		}
		return &ctlDisconnecter{path: cfg.CTLPath, run: run, now: time.Now, limit: 10 * time.Second}, nil
	case "rest":
		if rest == nil || !rest.Enabled() {
			return nil, errors.New("EMQX REST disconnect mode requires rest_url, api_key and api_secret")
		}
		return rest, nil
	default:
		return nil, fmt.Errorf("unsupported emqx.disconnect_mode %q", cfg.DisconnectMode)
	}
}

func (d *ctlDisconnecter) Disconnect(ctx context.Context, clientID string) error {
	if clientID == "" {
		return errors.New("client id is required")
	}
	timeout := d.limit
	if timeout <= 0 {
		timeout = 10 * time.Second
	}
	ctx, cancel := context.WithTimeout(ctx, timeout)
	defer cancel()
	out, err := d.run(ctx, d.path, "ctl", "clients", "kick", clientID)
	if err != nil {
		// EMQX returns success when the client is already gone; a not-found
		// style message is still an effective disconnect.
		text := strings.ToLower(string(out))
		if strings.Contains(text, "not_found") || strings.Contains(text, "not found") {
			return nil
		}
		return fmt.Errorf("emqx ctl kick: %w", err)
	}
	return nil
}

// DisconnectDevice implements auth.Transport's explicit device disconnect.
func (s *Service) DisconnectDevice(ctx context.Context, deviceID string) error {
	if s.disconnecter == nil {
		return errors.New("broker: no disconnecter configured")
	}
	return s.disconnecter.Disconnect(ctx, ClientIDForDevice(deviceID))
}
