package store

import (
	"errors"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
)

// Host and device rows are the only place where persisted protocol versions are
// narrowed from int32 to uint32, and where a missing row becomes ErrNotFound.
// A scan error must keep its own identity instead of being mistaken for absence.
func TestScanHostMapsPersistedRowAndPreservesErrorTaxonomy(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	row := syntheticRow{fill: func(dest ...any) {
		*dest[0].(*string) = "host"
		*dest[1].(*string) = "fingerprint"
		*dest[2].(*int32) = 3
		*dest[3].(*time.Time) = at
		*dest[4].(*time.Time) = at.Add(time.Second)
	}}
	host, err := scanHost(row)
	if err != nil {
		t.Fatalf("scan host: %v", err)
	}
	if host.HostID != "host" || host.Fingerprint != "fingerprint" || host.ProtocolVersion != 3 ||
		!host.CreatedAt.Equal(at) || !host.UpdatedAt.Equal(at.Add(time.Second)) {
		t.Fatalf("host row was not mapped: %+v", host)
	}

	if _, err := scanHost(syntheticRow{err: pgx.ErrNoRows}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing host row must map to ErrNotFound, got %v", err)
	}
	storageErr := errors.New("synthetic scan failure")
	if _, err := scanHost(syntheticRow{err: storageErr}); !errors.Is(err, storageErr) {
		t.Fatalf("storage failure must keep its identity, got %v", err)
	}
}

func TestScanDeviceMapsPersistedRowAndPreservesErrorTaxonomy(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	authHash := []byte{1, 2, 3}
	row := syntheticRow{fill: func(dest ...any) {
		*dest[0].(*string) = "device"
		*dest[1].(*string) = "public-key-fingerprint"
		*dest[2].(*[]byte) = authHash
		*dest[3].(*time.Time) = at
		*dest[4].(*time.Time) = at.Add(time.Second)
	}}
	device, err := scanDevice(row)
	if err != nil {
		t.Fatalf("scan device: %v", err)
	}
	if device.DeviceID != "device" || device.PublicKeyFingerprint != "public-key-fingerprint" ||
		string(device.AuthHash) != string(authHash) || !device.CreatedAt.Equal(at) {
		t.Fatalf("device row was not mapped: %+v", device)
	}

	if _, err := scanDevice(syntheticRow{err: pgx.ErrNoRows}); !errors.Is(err, ErrNotFound) {
		t.Fatalf("missing device row must map to ErrNotFound, got %v", err)
	}
	storageErr := errors.New("synthetic scan failure")
	if _, err := scanDevice(syntheticRow{err: storageErr}); !errors.Is(err, storageErr) {
		t.Fatalf("storage failure must keep its identity, got %v", err)
	}
}
