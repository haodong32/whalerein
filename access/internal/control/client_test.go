package control

import (
	"context"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync"
	"testing"
	"time"

	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
	"google.golang.org/protobuf/encoding/protojson"
)

func clientFixture(t *testing.T, handler http.HandlerFunc) (*Client, *httptest.Server) {
	t.Helper()
	server := httptest.NewServer(handler)
	t.Cleanup(server.Close)
	client, err := New(Config{
		BaseURL: server.URL, Token: "node-token", NodeID: "node-a",
		Incarnation: "inc-1", RegionID: "region", HTTPSURL: "https://a", MQTTURL: "mqtts://a", Capacity: 4,
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	return client, server
}

func TestRegisterAndReportUseTheNodeCredential(t *testing.T) {
	var registered whalereinv1.NodeIdentity
	var reports []*whalereinv1.NodeReport
	var lock sync.Mutex
	client, _ := clientFixture(t, func(w http.ResponseWriter, r *http.Request) {
		lock.Lock()
		defer lock.Unlock()
		if r.Header.Get("Authorization") != "Bearer node-token" {
			t.Errorf("authorization = %q", r.Header.Get("Authorization"))
		}
		raw, _ := io.ReadAll(r.Body)
		switch r.URL.Path {
		case "/api/v1/nodes/register":
			if err := protojson.Unmarshal(raw, &registered); err != nil {
				t.Error(err)
			}
		case "/api/v1/nodes/reports":
			report := &whalereinv1.NodeReport{}
			if err := protojson.Unmarshal(raw, report); err != nil {
				t.Error(err)
			}
			reports = append(reports, report)
		default:
			t.Errorf("unexpected path %s", r.URL.Path)
		}
		w.WriteHeader(http.StatusNoContent)
	})
	ctx := context.Background()
	if err := client.Register(ctx); err != nil {
		t.Fatal(err)
	}
	if registered.GetNodeId() != "node-a" || registered.GetIncarnation() != "inc-1" || registered.GetCapacity() != 4 {
		t.Fatalf("registered = %s/%s/%d", registered.GetNodeId(), registered.GetIncarnation(), registered.GetCapacity())
	}
	if err := client.Report(ctx, true, 2); err != nil {
		t.Fatal(err)
	}
	if err := client.Report(ctx, false, 3); err != nil {
		t.Fatal(err)
	}
	lock.Lock()
	defer lock.Unlock()
	if len(reports) != 2 || reports[0].GetSequence() != 1 || reports[1].GetSequence() != 2 {
		t.Fatalf("reports = %#v", reports)
	}
	if !reports[0].GetReady() || reports[1].GetReady() || reports[1].GetHostCount() != 3 {
		t.Fatalf("report fields = %#v", reports)
	}
}

func TestControlErrorsAreReported(t *testing.T) {
	client, server := clientFixture(t, func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "denied", http.StatusUnauthorized)
	})
	if err := client.Register(context.Background()); err == nil || !strings.Contains(err.Error(), "401") {
		t.Fatalf("register error = %v", err)
	}
	server.Close()
	if err := client.Report(context.Background(), true, 0); err == nil {
		t.Fatal("report against a closed server succeeded")
	}
}

func TestNewRequiresCompleteIdentity(t *testing.T) {
	for _, cfg := range []Config{{}, {BaseURL: "https://x"}, {BaseURL: "https://x", Token: "t"}, {BaseURL: "https://x", Token: "t", NodeID: "n"}} {
		if _, err := New(cfg, nil); err == nil {
			t.Fatalf("incomplete config accepted: %#v", cfg)
		}
	}
}

func TestRunStopsWithContext(t *testing.T) {
	var count int
	var lock sync.Mutex
	client, _ := clientFixture(t, func(w http.ResponseWriter, r *http.Request) {
		lock.Lock()
		count++
		lock.Unlock()
		w.WriteHeader(http.StatusNoContent)
	})
	ctx, cancel := context.WithCancel(context.Background())
	done := make(chan struct{})
	go func() {
		defer close(done)
		client.Run(ctx, time.Millisecond)
	}()
	time.Sleep(20 * time.Millisecond)
	cancel()
	select {
	case <-done:
	case <-time.After(time.Second):
		t.Fatal("reporter did not stop")
	}
	lock.Lock()
	defer lock.Unlock()
	if count == 0 {
		t.Fatal("no control messages sent")
	}
}

func TestInvalidBaseURLFailsOnRequest(t *testing.T) {
	client, err := New(Config{BaseURL: "://bad", Token: "t", NodeID: "n", Incarnation: "i"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := client.Register(context.Background()); err == nil {
		t.Fatal("invalid base URL accepted")
	}
}

func TestRunRetriesFailuresUntilStopped(t *testing.T) {
	client, _ := clientFixture(t, func(w http.ResponseWriter, r *http.Request) {
		http.Error(w, "unavailable", http.StatusServiceUnavailable)
	})
	ctx, cancel := context.WithTimeout(context.Background(), 20*time.Millisecond)
	defer cancel()
	client.Run(ctx, time.Millisecond)
}

func TestReportUsesTheDurableSequence(t *testing.T) {
	var sequence uint64 = 40
	var seen uint64
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		report := &whalereinv1.NodeReport{}
		raw, _ := io.ReadAll(r.Body)
		_ = protojson.Unmarshal(raw, report)
		seen = report.GetSequence()
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	client, err := NewWithSequence(Config{BaseURL: server.URL, Token: "t", NodeID: "n", Incarnation: "i"}, nil, func(context.Context) (uint64, error) {
		sequence++
		return sequence, nil
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := client.Report(context.Background(), true, 0); err != nil {
		t.Fatal(err)
	}
	if seen != 41 {
		t.Fatalf("sequence = %d, want 41", seen)
	}
	failure, err := NewWithSequence(Config{BaseURL: server.URL, Token: "t", NodeID: "n", Incarnation: "i"}, nil, func(context.Context) (uint64, error) {
		return 0, io.ErrUnexpectedEOF
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := failure.Report(context.Background(), true, 0); err == nil {
		t.Fatal("sequence failure was ignored")
	}
}

func TestAttachAndClosePostTheirScopedBodies(t *testing.T) {
	var paths []string
	client, _ := clientFixture(t, func(w http.ResponseWriter, r *http.Request) {
		paths = append(paths, r.URL.Path)
		w.WriteHeader(http.StatusNoContent)
	})
	if err := client.Attach(context.Background(), &whalereinv1.HostRouteAttach{HostId: "host"}); err != nil {
		t.Fatal(err)
	}
	if err := client.Close(context.Background(), &whalereinv1.HostRouteClose{HostId: "host"}); err != nil {
		t.Fatal(err)
	}
	if len(paths) != 2 || paths[0] != "/api/v1/nodes/hosts/attach" || paths[1] != "/api/v1/nodes/hosts/close" {
		t.Fatalf("paths = %v", paths)
	}
	if err := client.Attach(context.Background(), nil); err == nil {
		t.Fatal("nil attach accepted")
	}
	if err := client.Close(context.Background(), &whalereinv1.HostRouteClose{}); err == nil {
		t.Fatal("empty close accepted")
	}
	if err := client.Drain(context.Background(), "node", "inc", true); err != nil {
		t.Fatal(err)
	}
	if err := client.Drain(context.Background(), "", "inc", true); err == nil {
		t.Fatal("empty drain accepted")
	}
}

func TestRunRecoversWhenInitialRegistrationFailed(t *testing.T) {
	var registered bool
	var reports, registrations int
	client, _ := clientFixture(t, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/v1/nodes/register":
			registrations++
			if registrations == 1 {
				http.Error(w, "starting", http.StatusServiceUnavailable)
				return
			}
			registered = true
			w.WriteHeader(http.StatusNoContent)
		case "/api/v1/nodes/reports":
			if !registered {
				http.Error(w, "not registered", http.StatusNotFound)
				return
			}
			reports++
			w.WriteHeader(http.StatusNoContent)
		}
	})
	ctx, cancel := context.WithTimeout(context.Background(), 40*time.Millisecond)
	defer cancel()
	client.Run(ctx, 5*time.Millisecond)
	if reports == 0 {
		t.Fatal("report never recovered after registration")
	}
}

func TestSuccessfulControlMessageUpdatesTheMetricCallback(t *testing.T) {
	var observed time.Time
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.WriteHeader(http.StatusNoContent)
	}))
	defer server.Close()
	client, err := New(Config{
		BaseURL: server.URL, Token: "t", NodeID: "n", Incarnation: "i",
		OnSuccess: func(at time.Time) { observed = at },
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if err := client.Report(context.Background(), true, 0); err != nil {
		t.Fatal(err)
	}
	if observed.IsZero() {
		t.Fatal("success callback was not invoked")
	}
}

func TestInventoryDecodesAckAndRejectsMalformedResponses(t *testing.T) {
	malformed := true
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if malformed {
			_, _ = w.Write([]byte("not-json"))
			return
		}
		_, _ = w.Write([]byte(`{"inventory_sequence":"7"}`))
	}))
	defer server.Close()
	client, err := New(Config{BaseURL: server.URL, Token: "t", NodeID: "n", Incarnation: "i"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := client.Inventory(context.Background(), "", "i", nil); err == nil {
		t.Fatal("empty inventory scope accepted")
	}
	if _, err := client.Inventory(context.Background(), "n", "i", nil); err == nil {
		t.Fatal("malformed inventory response accepted")
	}
	malformed = false
	ack, err := client.Inventory(context.Background(), "n", "i", nil)
	if err != nil {
		t.Fatal(err)
	}
	if ack.GetInventorySequence() != 7 {
		t.Fatalf("sequence = %d", ack.GetInventorySequence())
	}
}

func TestAuthorityDecodesPermitAndRejectsEmptyResponses(t *testing.T) {
	empty := true
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		if empty {
			_, _ = w.Write([]byte(`{}`))
			return
		}
		_, _ = w.Write([]byte(`{"authority":{"authority_id":"a-1","session_id":"s-1"}}`))
	}))
	defer server.Close()
	client, err := New(Config{BaseURL: server.URL, Token: "t", NodeID: "n", Incarnation: "i"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := client.Authority(context.Background(), &whalereinv1.AuthorityRenewalRequest{HostId: "h"}); err == nil {
		t.Fatal("empty authority response accepted")
	}
	empty = false
	permit, err := client.Authority(context.Background(), &whalereinv1.AuthorityRenewalRequest{HostId: "h"})
	if err != nil || permit.GetAuthorityId() != "a-1" {
		t.Fatalf("permit = %#v %v", permit, err)
	}
}
