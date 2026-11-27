// Package control is Access's authenticated control client for Central.
//
// It reports content-free node health and registers the node incarnation. It
// never uploads request bodies, queue rows, temporary ciphertext or message
// content. A failure is logged and retried; it never changes local authority.
package control

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"log/slog"
	"net/http"
	"sync/atomic"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
	"google.golang.org/protobuf/proto"
)

// DefaultInterval is the required node heartbeat interval.
const DefaultInterval = 10 * time.Second

// Config identifies one node incarnation.
type Config struct {
	BaseURL     string
	Token       string
	NodeID      string
	Incarnation string
	RegionID    string
	HTTPSURL    string
	MQTTURL     string
	Capacity    uint32
	// OnSuccess observes each accepted Central response. It must not block.
	OnSuccess func(time.Time)
}

// Client posts node control messages over HTTPS.
type Client struct {
	config    Config
	http      *http.Client
	logger    *slog.Logger
	clock     func() time.Time
	seq       atomic.Uint64
	onSuccess func(time.Time)
	// nextSequence reads the durable sequence. Nil keeps an in-process
	// counter for a caller that has no local database.
	nextSequence func(context.Context) (uint64, error)
}

// New returns a control client.
func New(config Config, logger *slog.Logger) (*Client, error) {
	return NewWithSequence(config, logger, nil)
}

// NewWithSequence injects the durable sequence store.
func NewWithSequence(config Config, logger *slog.Logger, next func(context.Context) (uint64, error)) (*Client, error) {
	if config.BaseURL == "" || config.Token == "" || config.NodeID == "" || config.Incarnation == "" {
		return nil, errors.New("control: base URL, token, node id and incarnation are required")
	}
	if logger == nil {
		logger = slog.Default()
	}
	return &Client{config: config, http: &http.Client{Timeout: 5 * time.Second}, logger: logger,
		clock: func() time.Time { return time.Now().UTC() }, nextSequence: next,
		onSuccess: config.OnSuccess}, nil
}

// Register records the node incarnation and advertised addresses before the
// first report. Central must have a matching per-node credential.
func (c *Client) Register(ctx context.Context) error {
	message := &whalereinv1.NodeIdentity{
		NodeId: c.config.NodeID, Incarnation: c.config.Incarnation,
		RegionId: c.config.RegionID, HttpsUrl: c.config.HTTPSURL,
		MqttUrl: c.config.MQTTURL, Capacity: c.config.Capacity,
		RegisteredAtUnixMs: c.clock().UnixMilli(),
	}
	return c.post(ctx, "/api/v1/nodes/register", message)
}

// Attach forwards one verified Host route proof to Central.
func (c *Client) Attach(ctx context.Context, attach *whalereinv1.HostRouteAttach) error {
	if attach == nil || attach.GetHostId() == "" {
		return errors.New("control: attach scope is required")
	}
	return c.post(ctx, "/api/v1/nodes/hosts/attach", attach)
}

// Close forwards one terminal Host route close to Central.
func (c *Client) Close(ctx context.Context, close *whalereinv1.HostRouteClose) error {
	if close == nil || close.GetHostId() == "" {
		return errors.New("control: close scope is required")
	}
	return c.post(ctx, "/api/v1/nodes/hosts/close", close)
}

// Drain changes this node's operator drain flag. Access marks itself drained
// before a graceful stop so Central stops new placements immediately.
func (c *Client) Drain(ctx context.Context, nodeID, incarnation string, drained bool) error {
	if nodeID == "" || incarnation == "" {
		return errors.New("control: drain scope is required")
	}
	return c.post(ctx, "/api/v1/nodes/drain", &whalereinv1.NodeDrain{
		NodeId: nodeID, Incarnation: incarnation, Drained: drained,
	})
}

// Inventory reports the terminal routes Access knows about so Central can
// reconcile a close that was committed locally while Central was unreachable.
func (c *Client) Inventory(ctx context.Context, nodeID, incarnation string, routes []*whalereinv1.HostRoute) (*whalereinv1.NodeInventoryAck, error) {
	if nodeID == "" || incarnation == "" {
		return nil, errors.New("control: inventory scope is required")
	}
	ack := &whalereinv1.NodeInventoryAck{}
	message := &whalereinv1.NodeInventory{NodeId: nodeID, Incarnation: incarnation, Routes: routes}
	if err := c.postDecode(ctx, "/api/v1/nodes/inventory", message, ack); err != nil {
		return nil, err
	}
	return ack, nil
}

// Authority asks Central to renew one App-session permit. Central remains the
// only signer; Access never mints or extends authority locally.
func (c *Client) Authority(ctx context.Context, request *whalereinv1.AuthorityRenewalRequest) (*whalereinv1.ScopedAuthority, error) {
	response := &whalereinv1.AuthorityRenewalResponse{}
	if err := c.postDecode(ctx, "/api/v1/nodes/authority", request, response); err != nil {
		return nil, err
	}
	if response.GetAuthority() == nil {
		return nil, errors.New("control: authority response is empty")
	}
	return response.GetAuthority(), nil
}

// Report sends one heartbeat. The sequence is strictly increasing for the
// lifetime of this process; Central ignores stale sequences.
func (c *Client) Report(ctx context.Context, ready bool, hostCount uint32) error {
	sequence := c.seq.Add(1)
	if c.nextSequence != nil {
		durable, err := c.nextSequence(ctx)
		if err != nil {
			return err
		}
		sequence = durable
	}
	message := &whalereinv1.NodeReport{
		NodeId: c.config.NodeID, Incarnation: c.config.Incarnation,
		Sequence: sequence, Ready: ready, Capacity: c.config.Capacity,
		HostCount: hostCount, ObservedAtUnixMs: c.clock().UnixMilli(),
	}
	return c.post(ctx, "/api/v1/nodes/reports", message)
}

// Run registers once and then reports until the context ends. A registration
// or report failure is retried on the next interval; it is never fatal.
func (c *Client) Run(ctx context.Context, interval time.Duration) {
	if interval <= 0 {
		interval = DefaultInterval
	}
	if err := c.Register(ctx); err != nil {
		c.logger.Warn("central node registration failed", "error", err.Error())
	}
	ticker := time.NewTicker(interval)
	defer ticker.Stop()
	for {
		if err := c.Report(ctx, true, 0); err != nil {
			c.logger.Warn("central node report failed", "error", err.Error())
			// A node that started while Central was down never registered, so
			// every report would fail forever. Re-register and retry once;
			// both operations are idempotent for this incarnation.
			if registerErr := c.Register(ctx); registerErr == nil {
				if retryErr := c.Report(ctx, true, 0); retryErr != nil {
					c.logger.Warn("central node report retry failed", "error", retryErr.Error())
				}
			}
		}
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		}
	}
}

func (c *Client) do(ctx context.Context, path string, message proto.Message) ([]byte, error) {
	raw, err := protojson.Marshal(message)
	if err != nil {
		return nil, fmt.Errorf("control: encode %s: %w", path, err)
	}
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, c.config.BaseURL+path, bytes.NewReader(raw))
	if err != nil {
		return nil, err
	}
	request.Header.Set("Authorization", "Bearer "+c.config.Token)
	request.Header.Set("Content-Type", "application/json")
	response, err := c.http.Do(request)
	if err != nil {
		return nil, err
	}
	defer func() { _ = response.Body.Close() }()
	if response.StatusCode/100 != 2 {
		body, _ := io.ReadAll(io.LimitReader(response.Body, 512))
		return nil, fmt.Errorf("control: %s returned %d: %s", path, response.StatusCode, string(body))
	}
	body, err := io.ReadAll(io.LimitReader(response.Body, 1<<20))
	if err != nil {
		return nil, fmt.Errorf("control: read %s: %w", path, err)
	}
	if c.onSuccess != nil {
		c.onSuccess(time.Now().UTC())
	}
	return body, nil
}

func (c *Client) postDecode(ctx context.Context, path string, message, out proto.Message) error {
	body, err := c.do(ctx, path, message)
	if err != nil {
		return err
	}
	if err := protojson.Unmarshal(body, out); err != nil {
		return fmt.Errorf("control: decode %s: %w", path, err)
	}
	return nil
}

func (c *Client) post(ctx context.Context, path string, message proto.Message) error {
	_, err := c.do(ctx, path, message)
	return err
}
