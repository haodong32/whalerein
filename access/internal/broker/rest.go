package broker

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"os"
	"strings"
	"time"

	"github.com/whalerein/whalerein/access/internal/config"
)

// RESTClient is a minimal EMQX v5 management API client used for presence
// queries and active disconnects. It is optional; $SYS presence and the
// emqx ctl kicker work without an API key.
type RESTClient struct {
	base   *url.URL
	key    string
	secret string
	client *http.Client
}

// NewRESTClient builds a client from configuration. It returns (nil, nil) when
// no REST URL or API key is configured.
func NewRESTClient(cfg config.EMQXConfig) (*RESTClient, error) {
	if strings.TrimSpace(cfg.RESTURL) == "" {
		return nil, nil
	}
	base, err := url.Parse(cfg.RESTURL)
	if err != nil || (base.Scheme != "http" && base.Scheme != "https") || base.Host == "" {
		return nil, errors.New("emqx.rest_url must be an absolute http(s) URL")
	}
	transport := http.DefaultTransport.(*http.Transport).Clone()
	if base.Scheme == "https" {
		tlsConfig, err := tlsConfigFor(cfg.CAFile, cfg.InsecureSkipVerify, base.Hostname())
		if err != nil {
			return nil, fmt.Errorf("emqx TLS: %w", err)
		}
		transport.TLSClientConfig = tlsConfig
	}
	timeout := cfg.Timeout.D()
	if timeout <= 0 {
		timeout = 5 * time.Second
	}
	return &RESTClient{
		base:   base,
		key:    cfg.APIKey,
		secret: cfg.APISecret,
		client: &http.Client{Timeout: timeout, Transport: transport},
	}, nil
}

// Enabled reports whether the EMQX REST client is usable.
func (c *RESTClient) Enabled() bool {
	return c != nil && c.key != "" && c.secret != ""
}

// ClientConnected asks EMQX whether the client id is currently connected. A
// 404 is reported as (false, nil).
func (c *RESTClient) ClientConnected(ctx context.Context, clientID string) (bool, error) {
	if !c.Enabled() {
		return false, errors.New("emqx REST API credentials are not configured")
	}
	req, err := c.request(ctx, http.MethodGet, "/api/v5/clients/"+url.PathEscape(clientID), nil)
	if err != nil {
		return false, err
	}
	resp, err := c.client.Do(req)
	if err != nil {
		return false, fmt.Errorf("query EMQX client: %w", err)
	}
	defer resp.Body.Close()
	body, _ := io.ReadAll(io.LimitReader(resp.Body, 1<<20))
	switch resp.StatusCode {
	case http.StatusNotFound:
		return false, nil
	case http.StatusOK:
	default:
		return false, fmt.Errorf("query EMQX client: http %d", resp.StatusCode)
	}
	return parseConnected(body)
}

func parseConnected(body []byte) (bool, error) {
	var raw struct {
		Connected *bool           `json:"connected"`
		Data      json.RawMessage `json:"data"`
	}
	if err := json.Unmarshal(body, &raw); err != nil {
		return false, fmt.Errorf("decode EMQX client: %w", err)
	}
	if raw.Connected != nil {
		return *raw.Connected, nil
	}
	if len(raw.Data) > 0 {
		var inner struct {
			Connected *bool `json:"connected"`
		}
		if err := json.Unmarshal(raw.Data, &inner); err == nil && inner.Connected != nil {
			return *inner.Connected, nil
		}
	}
	return false, nil
}

// Disconnect satisfies Disconnecter.
func (c *RESTClient) Disconnect(ctx context.Context, clientID string) error {
	return c.DisconnectClient(ctx, clientID)
}

// DisconnectClient actively disconnects one client id.
func (c *RESTClient) DisconnectClient(ctx context.Context, clientID string) error {
	if !c.Enabled() {
		return errors.New("emqx REST API credentials are not configured")
	}
	req, err := c.request(ctx, http.MethodDelete, "/api/v5/clients/"+url.PathEscape(clientID), nil)
	if err != nil {
		return err
	}
	resp, err := c.client.Do(req)
	if err != nil {
		return fmt.Errorf("disconnect EMQX client: %w", err)
	}
	defer resp.Body.Close()
	_, _ = io.Copy(io.Discard, io.LimitReader(resp.Body, 1<<16))
	if resp.StatusCode == http.StatusNotFound || resp.StatusCode == http.StatusNoContent || resp.StatusCode == http.StatusOK {
		return nil
	}
	return fmt.Errorf("disconnect EMQX client: http %d", resp.StatusCode)
}

func (c *RESTClient) request(ctx context.Context, method, path string, body io.Reader) (*http.Request, error) {
	u := *c.base
	u.Path = strings.TrimRight(u.Path, "/") + path
	req, err := http.NewRequestWithContext(ctx, method, u.String(), body)
	if err != nil {
		return nil, fmt.Errorf("build EMQX request: %w", err)
	}
	req.SetBasicAuth(c.key, c.secret)
	req.Header.Set("Accept", "application/json")
	return req, nil
}

func tlsConfigFor(caFile string, insecure bool, serverName string) (*tls.Config, error) {
	cfg := &tls.Config{ // #nosec G402 -- insecure is an explicit local/test config switch.
		MinVersion:         tls.VersionTLS12,
		ServerName:         serverName,
		InsecureSkipVerify: insecure,
	}
	if caFile != "" {
		pem, err := os.ReadFile(caFile)
		if err != nil {
			return nil, fmt.Errorf("read CA file: %w", err)
		}
		pool, err := x509.SystemCertPool()
		if err != nil || pool == nil {
			pool = x509.NewCertPool()
		}
		if !pool.AppendCertsFromPEM(pem) {
			return nil, errors.New("CA file contains no certificates")
		}
		cfg.RootCAs = pool
	}
	return cfg, nil
}
