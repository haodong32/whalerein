package store_test

import (
	"context"
	"crypto/sha256"
	"errors"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"github.com/whalerein/whalerein/access/internal/store"
)

func openDirectoryStore(t *testing.T) (*store.Store, context.Context) {
	t.Helper()
	ctx := context.Background()
	data, err := store.Open(ctx, filepath.Join(t.TempDir(), "access.db"))
	if err != nil {
		t.Fatalf("open sqlite: %v", err)
	}
	t.Cleanup(data.Close)
	if err := data.Migrate(ctx); err != nil {
		t.Fatalf("migrate sqlite: %v", err)
	}
	return data, ctx
}

func invitationHash(token string) []byte {
	digest := sha256.Sum256([]byte(token))
	return digest[:]
}

func TestHostDirectoryRequiresAnExactUnusedInvitation(t *testing.T) {
	data, ctx := openDirectoryStore(t)
	now := time.Now().UTC().Truncate(time.Millisecond)
	for _, hostID := range []string{"host-1", "host-2"} {
		if _, err := data.EnsureHost(ctx, hostID); err != nil {
			t.Fatal(err)
		}
		if err := data.SetHostDirectory(ctx, hostID, "Directory "+hostID, "linux"); err != nil {
			t.Fatal(err)
		}
	}
	validHash := invitationHash("valid-invitation")
	if err := data.CreatePairingToken(ctx, validHash, "host-1", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}

	// A valid, unused, unexpired invitation for the requested Host returns the
	// stored fields.
	directory, err := data.HostDirectory(ctx, "host-1", validHash, now)
	if err != nil {
		t.Fatalf("valid directory read = %v", err)
	}
	if directory.HostID != "host-1" || directory.DisplayName != "Directory host-1" || directory.OSFamily != "linux" {
		t.Fatalf("directory = %#v", directory)
	}

	// The read is metadata-only: it creates no grant or account state and does
	// not consume the invitation.
	token, err := data.PairingTokenByHash(ctx, validHash)
	if err != nil {
		t.Fatalf("token after read = %v", err)
	}
	if token.UsedAt != nil {
		t.Fatalf("directory read consumed the invitation: %#v", token)
	}
	grants, err := data.ListHostGrants(ctx, "host-1")
	if err != nil {
		t.Fatal(err)
	}
	if len(grants) != 0 {
		t.Fatalf("directory read created grants: %#v", grants)
	}
	host, err := data.GetHost(ctx, "host-1")
	if err != nil {
		t.Fatal(err)
	}
	if host.HostID != "host-1" {
		t.Fatalf("host changed: %#v", host)
	}

	// A hash that was never issued, a token bound to another Host, a used
	// token and an expired token are all indistinguishable from a missing row.
	if _, err := data.HostDirectory(ctx, "host-1", invitationHash("unknown"), now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("unknown token = %v", err)
	}
	if _, err := data.HostDirectory(ctx, "host-2", validHash, now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("wrong host token = %v", err)
	}

	usedHash := invitationHash("used-invitation")
	if err := data.CreatePairingToken(ctx, usedHash, "host-1", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	if _, err := data.MarkPairingTokenUsed(ctx, usedHash, now); err != nil {
		t.Fatal(err)
	}
	if _, err := data.HostDirectory(ctx, "host-1", usedHash, now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("used token = %v", err)
	}

	expiredHash := invitationHash("expired-invitation")
	if err := data.CreatePairingToken(ctx, expiredHash, "host-1", now.Add(-time.Minute), now.Add(-2*time.Minute)); err != nil {
		t.Fatal(err)
	}
	if _, err := data.HostDirectory(ctx, "host-1", expiredHash, now); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("expired token = %v", err)
	}
}

func TestHostDirectoryReturnsEmptyFieldsBeforeHostReportsMetadata(t *testing.T) {
	data, ctx := openDirectoryStore(t)
	now := time.Now().UTC()
	if _, err := data.EnsureHost(ctx, "host-1"); err != nil {
		t.Fatal(err)
	}
	hash := invitationHash("unreported-invitation")
	if err := data.CreatePairingToken(ctx, hash, "host-1", now.Add(time.Hour), now); err != nil {
		t.Fatal(err)
	}
	directory, err := data.HostDirectory(ctx, "host-1", hash, now)
	if err != nil {
		t.Fatalf("empty directory read = %v", err)
	}
	if directory.HostID != "host-1" || directory.DisplayName != "" || directory.OSFamily != "" {
		t.Fatalf("directory = %#v", directory)
	}
}

func TestSetHostDirectoryValidatesBoundsAndControls(t *testing.T) {
	data, ctx := openDirectoryStore(t)
	if _, err := data.EnsureHost(ctx, "host-1"); err != nil {
		t.Fatal(err)
	}
	for name, tc := range map[string]struct {
		displayName string
		osFamily    string
	}{
		"display name too long": {strings.Repeat("a", 65), "linux"},
		"os family too long":    {"Host", strings.Repeat("b", 17)},
		"newline in display":    {"Host\nName", "linux"},
		"control in os family":  {"Host", "lin\tux"},
		"nul in display":        {"Host\x00Name", "linux"},
		"invalid utf8":          {string([]byte{0xff, 0xfe}), "linux"},
	} {
		t.Run(name, func(t *testing.T) {
			if err := data.SetHostDirectory(ctx, "host-1", tc.displayName, tc.osFamily); !errors.Is(err, store.ErrInvalidHostDirectory) {
				t.Fatalf("SetHostDirectory = %v", err)
			}
		})
	}
	if err := data.SetHostDirectory(ctx, "missing-host", "Host", "linux"); !errors.Is(err, store.ErrNotFound) {
		t.Fatalf("missing host = %v", err)
	}
	// Empty values are storable: the Host may clear its own display metadata.
	if err := data.SetHostDirectory(ctx, "host-1", "", ""); err != nil {
		t.Fatalf("empty values = %v", err)
	}
}
