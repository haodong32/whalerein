// Command server runs WhaleRein Access as one modular Go process: HTTPS API,
// internal EMQX callbacks, PostgreSQL state and one MQTT 5 session.
package main

import (
	"context"
	"errors"
	"flag"
	"fmt"
	"io"
	"log/slog"
	"net"
	"net/http"
	"os"
	"os/signal"
	"sync"
	"syscall"
	"time"

	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/events"
	"github.com/whalerein/whalerein/access/internal/httpapi"
	"github.com/whalerein/whalerein/access/internal/pairing"
	"github.com/whalerein/whalerein/access/internal/requests"
	"github.com/whalerein/whalerein/access/internal/store"
)

const version = "0.1.0"

func main() {
	os.Exit(entrypoint())
}

func entrypoint() int {
	ctx, stop := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer stop()
	return run(ctx, os.Args[1:], os.Stdout, os.Stderr)
}

type options struct {
	configPath  string
	checkConfig bool
	migrateOnly bool
	showVersion bool
}

func parseOptions(args []string, stderr io.Writer) (options, error) {
	var opts options
	fs := flag.NewFlagSet("whalerein-access", flag.ContinueOnError)
	fs.SetOutput(stderr)
	fs.StringVar(&opts.configPath, "config", "access/config.yaml", "path to the Access YAML config")
	fs.BoolVar(&opts.checkConfig, "check-config", false, "validate the config and exit")
	fs.BoolVar(&opts.migrateOnly, "migrate-only", false, "apply the schema migration and exit")
	fs.BoolVar(&opts.showVersion, "version", false, "print the Access version and exit")
	if err := fs.Parse(args); err != nil {
		return options{}, err
	}
	if fs.NArg() != 0 {
		return options{}, fmt.Errorf("unexpected positional arguments: %v", fs.Args())
	}
	return opts, nil
}

func run(ctx context.Context, args []string, stdout, stderr io.Writer) int {
	opts, err := parseOptions(args, stderr)
	if err != nil {
		fmt.Fprintf(stderr, "access: %v\n", err)
		return 2
	}
	if opts.showVersion {
		fmt.Fprintf(stdout, "whalerein-access %s\n", version)
		return 0
	}
	cfg, err := config.Load(opts.configPath)
	if err != nil {
		fmt.Fprintf(stderr, "access: invalid configuration: %v\n", err)
		return 2
	}
	if opts.checkConfig {
		mqttURL, derived := cfg.ResolveMQTTPublicURL()
		source := "configured"
		if derived {
			source = "derived"
		}
		fmt.Fprintf(stdout, "config ok: mode=%s listen=%s internal_listen=%s protocol=%d mqtt_public_url=%s (%s)\n",
			cfg.Mode, cfg.Listen, cfg.InternalListen, cfg.Protocol.Version, mqttURL, source)
		return 0
	}

	logger := diagnostics.NewLogger(cfg.Log.Level, stderr)
	if opts.migrateOnly {
		db, err := store.Open(ctx, cfg.DatabaseURL)
		if err != nil {
			logger.Error("migration failed", "error", err.Error())
			return 1
		}
		defer db.Close()
		if err := db.Migrate(ctx); err != nil {
			logger.Error("migration failed", "error", err.Error())
			return 1
		}
		logger.Info("migration complete")
		return 0
	}
	if err := runServer(ctx, cfg, logger); err != nil {
		logger.Error("access stopped with error", "error", err.Error())
		return 1
	}
	return 0
}

func runServer(ctx context.Context, cfg config.Config, logger *slog.Logger) error {
	metrics := diagnostics.NewMetrics()
	masker := diagnostics.NewMasker()

	db, err := store.Open(ctx, cfg.DatabaseURL)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	defer db.Close()
	if err := db.Migrate(ctx); err != nil {
		return fmt.Errorf("migrate store: %w", err)
	}

	mqttPublicURL, derived := cfg.ResolveMQTTPublicURL()
	if derived {
		logger.Warn("broker.public_url is not configured; deriving the pairing MQTT URL",
			"mqtt_public_url", mqttPublicURL)
	}

	rest, err := broker.NewRESTClient(cfg.EMQX)
	if err != nil {
		return fmt.Errorf("configure EMQX REST: %w", err)
	}
	disconnecter, err := broker.NewDisconnecter(cfg.EMQX, rest, nil)
	if err != nil {
		return fmt.Errorf("configure EMQX disconnect: %w", err)
	}
	presence := broker.NewPresence(cfg.Presence.CacheTTL.D(), rest, metrics, logger)

	var service *broker.Service
	connector, err := broker.NewPahoConnector(cfg.Broker, func(topic string, payload []byte) {
		service.HandleMessage(topic, payload)
	}, logger)
	if err != nil {
		return fmt.Errorf("configure broker: %w", err)
	}
	service, err = broker.New(broker.Dependencies{
		Config:       cfg.Broker,
		Presence:     presence,
		Connector:    connector,
		Disconnecter: disconnecter,
		Metrics:      metrics,
		Masker:       masker,
		Logger:       logger,
	})
	if err != nil {
		return err
	}

	authService, err := auth.New(auth.Dependencies{
		Store:     db,
		Presence:  service,
		Transport: service,
		Metrics:   metrics,
		Masker:    masker,
		Logger:    logger,
		Config: auth.Config{
			AdminToken:            cfg.AdminToken,
			HostProvisioningToken: cfg.HostProvisioningToken,
			AccessUsername:        cfg.Broker.Username,
			AccessPassword:        cfg.Broker.Password,
			ProtocolVersion:       cfg.Protocol.Version,
			RevocationAckTimeout:  cfg.Revocation.AckTimeout.D(),
		},
	})
	if err != nil {
		return err
	}

	requestService, err := requests.New(requests.Dependencies{
		Store:    db,
		Broker:   service,
		Presence: service,
		Config: requests.Config{
			ProtocolVersion: cfg.Protocol.Version,
			MaxTTL:          cfg.Requests.MaxTTL.D(),
			ForwardInterval: cfg.Requests.ForwardInterval.D(),
			Lease:           cfg.Requests.Lease.D(),
			RetryInterval:   cfg.Requests.RetryInterval.D(),
			MaxAttempts:     cfg.Requests.MaxAttempts,
			BatchSize:       cfg.Requests.BatchSize,
		},
		Metrics: metrics,
		Masker:  masker,
		Logger:  logger,
	})
	if err != nil {
		return err
	}

	eventService, err := events.New(events.Dependencies{
		Store: db, Broker: service, Metrics: metrics, Masker: masker, Logger: logger,
	})
	if err != nil {
		return err
	}

	pairingService, err := pairing.New(pairing.Dependencies{
		Store:     db,
		Transport: service,
		Presence:  service,
		Config: pairing.Config{
			PublicURL:       cfg.PublicURL,
			MQTTURL:         mqttPublicURL,
			InvitationTTL:   cfg.Pairing.InvitationTTL.D(),
			ClaimTimeout:    cfg.Pairing.ClaimTimeout.D(),
			ClaimTTL:        cfg.Pairing.ClaimTTL.D(),
			MaxFailures:     cfg.Pairing.MaxFailures,
			ProtocolVersion: cfg.Protocol.Version,
		},
		Metrics: metrics,
		Masker:  masker,
		Logger:  logger,
	})
	if err != nil {
		return err
	}

	service.SetResultHandler(requestService.HandleHostResult)
	service.SetEventHandler(eventService.Handle)
	service.SetReadyHandler(authService.ReplayPendingRevocations)
	service.SetHostKeyHandler(authService.RememberHostAuthorizationKey)

	readiness := diagnostics.NewReadiness()
	readiness.Add("database", func(ctx context.Context) error { return db.Ping(ctx) })
	readiness.Add("broker", func(context.Context) error {
		if !service.Ready() {
			return errors.New("mqtt session is not ready")
		}
		return nil
	})

	api, err := httpapi.New(httpapi.Dependencies{
		Auth:          authService,
		Pairing:       pairingService,
		Requests:      requestService,
		Presence:      presence,
		Readiness:     readiness,
		Metrics:       metrics,
		Masker:        masker,
		Logger:        logger,
		InternalToken: cfg.Internal.CallbackToken,
		MaxBodyBytes:  cfg.HTTP.MaxBodyBytes,
	})
	if err != nil {
		return err
	}

	logger.Info("access starting", "config", cfg.Redacted(), "version", version)

	publicListener, err := net.Listen("tcp", cfg.Listen)
	if err != nil {
		return fmt.Errorf("listen public: %w", err)
	}
	internalListener, err := net.Listen("tcp", cfg.InternalListen)
	if err != nil {
		_ = publicListener.Close()
		return fmt.Errorf("listen internal: %w", err)
	}

	publicServer := &http.Server{
		Handler:           api.PublicHandler(),
		ReadHeaderTimeout: cfg.HTTP.ReadHeaderTimeout.D(),
		ReadTimeout:       cfg.HTTP.ReadTimeout.D(),
		WriteTimeout:      cfg.HTTP.WriteTimeout.D(),
		IdleTimeout:       cfg.HTTP.IdleTimeout.D(),
	}
	internalServer := &http.Server{
		Handler:           api.InternalHandler(),
		ReadHeaderTimeout: cfg.HTTP.ReadHeaderTimeout.D(),
		ReadTimeout:       cfg.HTTP.ReadTimeout.D(),
		WriteTimeout:      cfg.HTTP.WriteTimeout.D(),
		IdleTimeout:       cfg.HTTP.IdleTimeout.D(),
	}

	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	// Start the HTTP listeners before the MQTT loop: EMQX calls the internal
	// auth/ACL endpoints while Access establishes its own broker connection.
	// If those endpoints are not listening yet, EMQX marks the auth resource
	// down and a fast Access restart can be denied for a whole retry window.
	errCh := make(chan error, 2)
	go serve(publicServer, publicListener, "public", errCh)
	go serve(internalServer, internalListener, "internal", errCh)
	logger.Info("access listening",
		"public", publicListener.Addr().String(),
		"internal", internalListener.Addr().String())

	var workers sync.WaitGroup
	workers.Add(2)
	go func() {
		defer workers.Done()
		if err := service.Run(runCtx); err != nil {
			logger.Warn("broker loop stopped", "error", err.Error())
		}
	}()
	go func() {
		defer workers.Done()
		if err := requestService.Run(runCtx); err != nil {
			logger.Warn("forward loop stopped", "error", err.Error())
		}
	}()
	go runCleanup(runCtx, db, cfg.Cleanup.Interval.D(), cfg.Cleanup.PairingTokenRetention.D(), logger)

	var runErr error
	select {
	case <-ctx.Done():
	case runErr = <-errCh:
		cancel()
	}

	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer shutdownCancel()
	shutdownErr := errors.Join(
		publicServer.Shutdown(shutdownCtx),
		internalServer.Shutdown(shutdownCtx),
	)
	cancel()
	waitWithTimeout(&workers, 10*time.Second)
	if runErr != nil {
		return runErr
	}
	if shutdownErr != nil {
		return fmt.Errorf("shutdown: %w", shutdownErr)
	}
	logger.Info("access stopped")
	return nil
}

func serve(server *http.Server, listener net.Listener, name string, errCh chan<- error) {
	if err := server.Serve(listener); err != nil && !errors.Is(err, http.ErrServerClosed) {
		select {
		case errCh <- fmt.Errorf("%s server: %w", name, err):
		default:
		}
	}
}

func waitWithTimeout(group *sync.WaitGroup, timeout time.Duration) {
	done := make(chan struct{})
	go func() {
		group.Wait()
		close(done)
	}()
	timer := time.NewTimer(timeout)
	defer timer.Stop()
	select {
	case <-done:
	case <-timer.C:
	}
}

func runCleanup(ctx context.Context, db *store.Store, interval, retention time.Duration, logger *slog.Logger) {
	if interval <= 0 {
		interval = time.Minute
	}
	if retention <= 0 {
		retention = 24 * time.Hour
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	cleanup := func() {
		now := time.Now().UTC()
		cleared, err := db.ClearExpiredBodies(ctx, now)
		if err != nil {
			logger.Warn("expired request body cleanup failed", "error", err.Error())
		} else if cleared > 0 {
			logger.Info("expired request bodies cleared", "count", cleared)
		}
		deleted, err := db.DeleteExpiredPairingTokens(ctx, now.Add(-retention))
		if err != nil {
			logger.Warn("pairing token cleanup failed", "error", err.Error())
		} else if deleted > 0 {
			logger.Info("expired pairing tokens deleted", "count", deleted)
		}
		// Read-only request identities have a finite window. Control identities
		// are never part of this sweep, even when their payloads have expired.
		pruned, err := db.DeleteExpiredQueries(ctx, now.Add(-24*time.Hour))
		if err != nil {
			logger.Warn("query record cleanup failed", "error", err.Error())
		} else if pruned > 0 {
			logger.Info("expired query records deleted", "count", pruned)
		}
	}
	cleanup()
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
			cleanup()
		}
	}
}
