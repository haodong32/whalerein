// Command server runs WhaleRein Access as one modular Go process: HTTPS API,
// internal broker callbacks, PostgreSQL state and one MQTT 5 session.
package main

import (
	"context"
	"crypto/ed25519"
	"encoding/base64"
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

	"github.com/whalerein/whalerein/access/internal/admission"
	"github.com/whalerein/whalerein/access/internal/auth"
	"github.com/whalerein/whalerein/access/internal/broker"
	"github.com/whalerein/whalerein/access/internal/config"
	"github.com/whalerein/whalerein/access/internal/diagnostics"
	"github.com/whalerein/whalerein/access/internal/events"
	"github.com/whalerein/whalerein/access/internal/external"
	"github.com/whalerein/whalerein/access/internal/httpapi"
	"github.com/whalerein/whalerein/access/internal/line"
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
	return runWith(ctx, args, stdout, stderr, defaultStartupStoreOps(), runServer)
}

func runWith(ctx context.Context, args []string, stdout, stderr io.Writer, ops startupStoreOps, serve func(context.Context, config.Config, *slog.Logger) error) int {
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
		effective, err := cfg.ResolveTransport()
		if err != nil {
			fmt.Fprintf(stderr, "access: invalid configuration: %v\n", err)
			return 2
		}
		fmt.Fprintf(stdout, "config ok: mode=%s listen=%s internal_listen=%s protocol=%d mqtt_public_url=%s (%s)\n",
			cfg.Mode, cfg.Listen, cfg.InternalListen, cfg.Protocol.Version, mqttURL, source)
		fmt.Fprintf(stdout, "transport: profile=%s packet_bytes=%d overhead_bytes=%d fragment_payload_bytes=%d fragment_count=%d envelope_bytes=%d bulk_concurrency=%d reserved_bytes=%d fragment_idle_timeout=%s transfer_timeout=%s result_limit_bytes=%d http_max_body_bytes=%d\n",
			effective.Profile, effective.PacketBytes, effective.PacketOverheadBytes,
			effective.FragmentPayloadBytes, effective.FragmentCount, effective.EnvelopeBytes,
			effective.BulkConcurrency, effective.ReservedBytes,
			effective.FragmentIdleTimeout, effective.TransferTimeout,
			effective.ResultLimitBytes, effective.HTTPBodyBytes)
		return 0
	}

	logger := diagnostics.NewLogger(cfg.Log.Level, stderr)
	if opts.migrateOnly {
		db, err := ops.open(ctx, cfg.DatabaseURL)
		if err != nil {
			logger.Error("migration failed", "error", err.Error())
			return 1
		}
		defer ops.close(db)
		if err := ops.migrate(ctx, db); err != nil {
			logger.Error("migration failed", "error", err.Error())
			return 1
		}
		logger.Info("migration complete")
		return 0
	}
	if err := serve(ctx, cfg, logger); err != nil {
		logger.Error("access stopped with error", "error", err.Error())
		return 1
	}
	return 0
}

type startupStoreOps struct {
	open    func(context.Context, string) (*store.Store, error)
	migrate func(context.Context, *store.Store) error
	close   func(*store.Store)
	cleanup func(context.Context, cleanupStore, time.Duration, time.Duration, *slog.Logger, *diagnostics.Metrics)
}

func defaultStartupStoreOps() startupStoreOps {
	return startupStoreOps{
		open:    store.Open,
		migrate: func(ctx context.Context, db *store.Store) error { return db.Migrate(ctx) },
		close:   func(db *store.Store) { db.Close() },
		cleanup: runCleanup,
	}
}

// externalTrust converts the configured issuer keys into the verification
// trust the external service uses.
func externalTrust(cfg config.Config) (external.Trust, error) {
	trust := external.Trust{
		Issuer:                   cfg.External.Issuer,
		ServiceIdentity:          cfg.External.ServiceIdentity,
		Region:                   cfg.External.Region,
		SessionEnvironment:       cfg.External.SessionEnvironment,
		SessionAuthorityRevision: cfg.External.SessionAuthorityRevision,
		ClockSkew:                cfg.External.ClockSkew.D(),
	}
	for _, key := range cfg.External.VerificationKeys {
		material, err := base64.RawURLEncoding.DecodeString(key.PublicKey)
		if err != nil {
			return external.Trust{}, fmt.Errorf("external key %s: %w", key.ID, err)
		}
		if len(material) != ed25519.PublicKeySize {
			return external.Trust{}, fmt.Errorf("external key %s is not an Ed25519 public key", key.ID)
		}
		trust.Keys = append(trust.Keys, external.VerificationKey{
			ID:        key.ID,
			PublicKey: ed25519.PublicKey(material),
			NotBefore: key.NotBefore.Time(),
			NotAfter:  key.NotAfter.Time(),
		})
	}
	return trust, nil
}

func runServer(ctx context.Context, cfg config.Config, logger *slog.Logger) error {
	return runServerWithStore(ctx, cfg, logger, defaultStartupStoreOps())
}

// runServerWithStore permits deterministic startup failure and cleanup checks.
func runServerWithStore(ctx context.Context, cfg config.Config, logger *slog.Logger, ops startupStoreOps) error {
	metrics := diagnostics.NewMetrics()
	masker := diagnostics.NewMasker()

	// Validate the packaged vocabulary and the effective budget before any
	// listener, database or broker connection becomes ready.
	transport, err := cfg.ResolveTransport()
	if err != nil {
		return fmt.Errorf("transport configuration: %w", err)
	}
	logger.Info("transport configured",
		"profile", transport.Profile,
		"packet_bytes", transport.PacketBytes,
		"fragment_payload_bytes", transport.FragmentPayloadBytes,
		"fragment_count", transport.FragmentCount,
		"envelope_bytes", transport.EnvelopeBytes,
		"bulk_concurrency", transport.BulkConcurrency,
		"reserved_bytes", transport.ReservedBytes,
		"fragment_idle_timeout_ms", transport.FragmentIdleTimeout.Milliseconds(),
		"transfer_timeout_ms", transport.TransferTimeout.Milliseconds(),
		"result_limit_bytes", transport.ResultLimitBytes,
		"http_max_body_bytes", transport.HTTPBodyBytes)

	db, err := ops.open(ctx, cfg.DatabaseURL)
	if err != nil {
		return fmt.Errorf("open store: %w", err)
	}
	defer ops.close(db)
	if err := ops.migrate(ctx, db); err != nil {
		return fmt.Errorf("migrate store: %w", err)
	}

	mqttPublicURL, derived := cfg.ResolveMQTTPublicURL()
	if derived {
		logger.Warn("broker.public_url is not configured; deriving the pairing MQTT URL",
			"mqtt_public_url", mqttPublicURL)
	}

	presence := broker.NewPresence(cfg.Presence.CacheTTL.D(), metrics, logger)

	var service *broker.Service
	connector, err := broker.NewPahoConnectorWithTransport(cfg.Broker, transport, metrics, func(topic string, payload []byte) {
		service.HandleMessage(topic, payload)
	}, logger)
	if err != nil {
		return fmt.Errorf("configure broker: %w", err)
	}
	service, err = broker.New(broker.Dependencies{
		Config:      cfg.Broker,
		Transport:   transport,
		Presence:    presence,
		Connector:   connector,
		DeviceHosts: db.ListAuthorizedHostIDs,
		Metrics:     metrics,
		Masker:      masker,
		Logger:      logger,
	})
	if err != nil {
		return err
	}

	admissionService, err := admission.New(admission.Dependencies{
		Store: db,
		Config: admission.Config{
			MaximumEntries:     cfg.Admission.MaxEntries,
			TTL:                cfg.Admission.TTL.D(),
			LoadTimeout:        cfg.Admission.LoadTimeout.D(),
			MaxBackgroundLoads: cfg.Admission.MaxBackgroundLoads,
		},
		Metrics: metrics,
		Masker:  masker,
		Logger:  logger,
	})
	if err != nil {
		return err
	}

	lineManager, err := line.New(line.Dependencies{
		Store:     db,
		Control:   service.Control(),
		Admission: admissionService,
		Presence:  presence,
		Config: line.Config{
			PolicyTTL: cfg.Line.PolicyTTL.D(),
			Timeout:   cfg.Line.OperationTimeout.D(),
		},
		Metrics: metrics,
		Masker:  masker,
		Logger:  logger,
	})
	if err != nil {
		return err
	}

	authDeps := auth.Dependencies{
		Store:     db,
		Admission: admissionService,
		Enforcer:  lineManager,
		Observer:  lineManager,
		Presence:  service,
		Transport: service,
		Metrics:   metrics,
		Masker:    masker,
		Logger:    logger,
		Config: auth.Config{
			AdminToken:               cfg.AdminToken,
			HostProvisioningToken:    cfg.HostProvisioningToken,
			AccessUsername:           cfg.Broker.Username,
			AccessPassword:           cfg.Broker.Password,
			ProtocolVersion:          cfg.Protocol.Version,
			RevocationAckTimeout:     cfg.Revocation.AckTimeout.D(),
			RequireHostEnrollment:    cfg.Mode == "external",
			RequireDeviceSession:     cfg.Mode == "external" && cfg.External.RequireDeviceSession,
			SessionAuthorityRevision: cfg.External.SessionAuthorityRevision,
		},
	}
	if cfg.Mode == "external" {
		authDeps.Sessions = db
	}
	authService, err := auth.New(authDeps)
	if err != nil {
		return err
	}

	requestDeps := requests.Dependencies{
		Store:    db,
		Broker:   service,
		Presence: service,
		Grants:   db,
		Config: requests.Config{
			SessionAuthorityRevision: cfg.External.SessionAuthorityRevision,
			ProtocolVersion:          cfg.Protocol.Version,
			MaxTTL:                   cfg.Requests.MaxTTL.D(),
			ForwardInterval:          cfg.Requests.ForwardInterval.D(),
			Lease:                    cfg.Requests.Lease.D(),
			RetryInterval:            cfg.Requests.RetryInterval.D(),
			MaxAttempts:              cfg.Requests.MaxAttempts,
			BatchSize:                cfg.Requests.BatchSize,
			MaxResultBytes:           transport.ResultLimitBytes,
			MaxDeliveryBytes:         transport.HTTPBodyBytes,
			EnvelopeBytes:            int64(transport.EnvelopeBytes),
		},
		Metrics: metrics,
		Masker:  masker,
		Logger:  logger,
	}
	if cfg.Mode == "external" {
		requestDeps.Sessions = db
	}
	requestService, err := requests.New(requestDeps)
	if err != nil {
		return err
	}

	eventService, err := events.New(events.Dependencies{
		Admission: admissionService, Broker: service, Metrics: metrics, Masker: masker, Logger: logger,
	})
	if err != nil {
		return err
	}

	pairingService, err := pairing.New(pairing.Dependencies{
		Store:     db,
		Policy:    lineManager,
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
	service.SetHostKeyHandler(authService.RememberHostAuthorizationKey)

	readiness := diagnostics.NewReadiness()
	// A ping-only check would keep a read-only replica or an exhausted pool in
	// rotation. The write probe is a no-op transaction that proves writability.
	readiness.Add("database", diagnostics.DatabaseReadiness(db))
	readiness.Add("broker", func(context.Context) error {
		if !service.Ready() {
			return errors.New("mqtt session is not ready")
		}
		if !presence.Available() {
			return errors.New("broker presence is not reconciled")
		}
		return nil
	})

	if cfg.Mode == "external" && cfg.External.SessionAuthorityRevision != "" {
		lineManager.SetSessionPolicy(authService.ManagedBrokerPolicy)
	}
	var externalService *external.Service
	if cfg.Mode == "external" {
		trust, err := externalTrust(cfg)
		if err != nil {
			return err
		}
		externalService = external.New(trust, db, cfg.External.MQTTURL,
			external.WithHostEnforcer(authService),
			external.WithDeviceGrants(pairingService, db, authService),
			external.WithHostClients(db, service, presence),
			external.WithDeviceSessions(db, service, cfg.External.RequireDeviceSession))
		logger.Info("external account mode enabled", "issuer", trust.Issuer, "region", trust.Region)
	}

	// HostReady is the only readiness source that advances an external
	// enrollment. A probe principal cannot publish a Result, so it never
	// reaches this handler, and the auth replay keeps its existing behavior.
	service.SetReadyHandler(func(ctx context.Context, hostID string) error {
		revocationErr := authService.ReplayPendingRevocations(ctx, hostID)
		if externalService == nil {
			return revocationErr
		}
		return errors.Join(revocationErr, externalService.HandleHostReady(ctx, hostID))
	})

	dependencies := httpapi.Dependencies{
		SessionAuthorityRevision: cfg.External.SessionAuthorityRevision,
		RequireDeviceSession:     cfg.Mode == "external" && cfg.External.RequireDeviceSession,
		HostPresence:             presence,
		Auth:                     authService,
		Pairing:                  pairingService,
		Requests:                 requestService,
		Readiness:                readiness,
		Metrics:                  metrics,
		Masker:                   masker,
		Logger:                   logger,
		InternalToken:            cfg.Internal.CallbackToken,
		MaxBodyBytes:             transport.HTTPBodyBytes,
	}
	// Assign through the interface only when external mode is configured. A
	// typed nil *external.Service would make the interface non-nil and turn the
	// documented 501 into a nil-pointer panic.
	if externalService != nil {
		dependencies.External = externalService
	}
	api, err := httpapi.New(dependencies)
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

	service.SetReadyHook(lineManager.Reconcile)
	return runServices(ctx, publicServer, internalServer, publicListener, internalListener,
		service.Run, requestService.Run,
		func(runCtx context.Context) error {
			return runLine(runCtx, lineManager, cfg, logger)
		},
		func(runCtx context.Context) {
			ops.cleanup(runCtx, db, cfg.Cleanup.Interval.D(), cfg.Cleanup.PairingTokenRetention.D(), logger, metrics)
		}, logger, 10*time.Second)
}

// runServices owns server and worker shutdown after resources are initialized.
func runServices(ctx context.Context, publicServer, internalServer *http.Server, publicListener, internalListener net.Listener,
	runBroker, runForwarder, runLineWorker func(context.Context) error, cleanup func(context.Context), logger *slog.Logger, shutdownTimeout time.Duration) error {
	runCtx, cancel := context.WithCancel(ctx)
	defer cancel()

	// Start the HTTP listeners before the MQTT loop: the broker plugin posts
	// CONNECT validation to the internal listener while Access establishes its
	// own broker connection, so the endpoint must already be serving.
	errCh := make(chan error, 2)
	go serve(publicServer, publicListener, "public", errCh)
	go serve(internalServer, internalListener, "internal", errCh)
	logger.Info("access listening",
		"public", publicListener.Addr().String(),
		"internal", internalListener.Addr().String())

	var workers sync.WaitGroup
	workers.Add(3)
	go func() {
		defer workers.Done()
		if err := runBroker(runCtx); err != nil {
			logger.Warn("broker loop stopped", "error", err.Error())
		}
	}()
	go func() {
		defer workers.Done()
		if err := runForwarder(runCtx); err != nil {
			logger.Warn("forward loop stopped", "error", err.Error())
		}
	}()
	go func() {
		defer workers.Done()
		if err := runLineWorker(runCtx); err != nil {
			logger.Warn("line loop stopped", "error", err.Error())
		}
	}()
	go cleanup(runCtx)

	var runErr error
	select {
	case <-ctx.Done():
	case runErr = <-errCh:
		cancel()
	}

	shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), shutdownTimeout)
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

type cleanupStore interface {
	ClearExpiredBodies(context.Context, time.Time) (int64, error)
	DeleteExpiredPairingTokens(context.Context, time.Time) (int64, error)
	DeleteExpiredQueries(context.Context, time.Time) (int64, error)
}

// maxCleanupRounds bounds one operation's catch-up work in a single tick, so a
// shutdown cannot be delayed indefinitely by a very large backlog.
const maxCleanupRounds = 100

// cleanupTask names one bounded retention statement and its operator-facing
// log lines.
type cleanupTask struct {
	operation string
	success   string
	failure   string
	// cutoff computes the retention boundary from the pass start time.
	cutoff func(time.Time) time.Time
	run    func(context.Context, time.Time) (int64, error)
}

func runCleanup(ctx context.Context, db cleanupStore, interval, retention time.Duration, logger *slog.Logger, metrics *diagnostics.Metrics) {
	if interval <= 0 {
		interval = time.Minute
	}
	if retention <= 0 {
		retention = 24 * time.Hour
	}
	tasks := []cleanupTask{
		{operation: "bodies", success: "expired request bodies cleared", failure: "expired request body cleanup failed",
			cutoff: func(now time.Time) time.Time { return now },
			run:    func(ctx context.Context, cutoff time.Time) (int64, error) { return db.ClearExpiredBodies(ctx, cutoff) }},
		{operation: "tokens", success: "expired pairing tokens deleted", failure: "pairing token cleanup failed",
			cutoff: func(now time.Time) time.Time { return now.Add(-retention) },
			run: func(ctx context.Context, cutoff time.Time) (int64, error) {
				return db.DeleteExpiredPairingTokens(ctx, cutoff)
			}},
		// Read-only request identities have a finite window. Control identities
		// are never part of this sweep, even when their payloads have expired.
		{operation: "queries", success: "expired query records deleted", failure: "query record cleanup failed",
			cutoff: func(now time.Time) time.Time { return now.Add(-24 * time.Hour) },
			run: func(ctx context.Context, cutoff time.Time) (int64, error) {
				return db.DeleteExpiredQueries(ctx, cutoff)
			}},
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	cleanup := func() {
		now := time.Now().UTC()
		for _, task := range tasks {
			drainCleanupTask(ctx, task, now, logger, metrics)
		}
	}
	// The first pass runs immediately: after a long pause (a restart or a
	// suspended worker) the backlog is drained without waiting one interval.
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

// drainCleanupTask repeats one bounded statement until it returns fewer rows
// than the batch bound. A full batch is the backlog signal: it is counted and
// logged so an alert can fire even though the work keeps making progress.
func drainCleanupTask(ctx context.Context, task cleanupTask, now time.Time, logger *slog.Logger, metrics *diagnostics.Metrics) {
	cutoff := task.cutoff(now)
	for round := 0; round < maxCleanupRounds; round++ {
		count, err := task.run(ctx, cutoff)
		if err != nil {
			logger.Warn(task.failure, "error", err.Error())
			return
		}
		metrics.CleanupBatch(task.operation, int(count))
		if count > 0 {
			logger.Info(task.success, "count", count, "operation", task.operation)
		}
		if count < store.CleanupBatchSize {
			metrics.CleanupCompleted(task.operation)
			return
		}
		metrics.CleanupBacklog()
		logger.Warn("retention cleanup batch reached its bound; more work remains", "operation", task.operation)
		if ctx.Err() != nil {
			return
		}
	}
	logger.Warn("retention cleanup stopped after the per-tick round limit", "operation", task.operation)
}

// runLine keeps the broker policy validity window fresh and periodically
// reconciles durable policy. A failed pass keeps the previous policy and the
// broker fails closed when the validity window elapses.
func runLine(ctx context.Context, manager *line.Manager, cfg config.Config, logger *slog.Logger) error {
	heartbeat := time.NewTicker(cfg.Line.HeartbeatInterval.D())
	defer heartbeat.Stop()
	reconcile := time.NewTicker(cfg.Line.ReconcileInterval.D())
	defer reconcile.Stop()
	failures := 0
	for {
		select {
		case <-ctx.Done():
			return nil
		case <-heartbeat.C:
			if err := manager.Heartbeat(ctx); err != nil {
				failures++
				if failures == 1 || failures%20 == 0 {
					logger.Warn("line policy heartbeat failed", "failures", failures, "error", err.Error())
				}
			} else {
				failures = 0
			}
		case <-reconcile.C:
			if err := manager.Reconcile(ctx); err != nil {
				logger.Warn("line policy reconcile failed", "error", err.Error())
			}
		}
	}
}
