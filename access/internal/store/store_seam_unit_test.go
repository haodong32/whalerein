package store

import (
	"context"
	"errors"
	"fmt"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

// fakeDB installs scripted database outcomes at the Store seam. Each hook
// models what PostgreSQL would return (rows, tags or errors); tests assert
// domain behavior rather than SQL text.
type fakeDB struct {
	t          *testing.T
	queryFn    func(context.Context, string, ...any) (pgx.Rows, error)
	queryRowFn func(context.Context, string, ...any) pgx.Row
	execFn     func(context.Context, string, ...any) (pgconn.CommandTag, error)
	beginFn    func(context.Context) (pgx.Tx, error)
	pingErr    error
	closed     int
}

func (db *fakeDB) Ping(context.Context) error { return db.pingErr }
func (db *fakeDB) Close()                     { db.closed++ }
func (db *fakeDB) Query(ctx context.Context, sql string, args ...any) (pgx.Rows, error) {
	if db.queryFn == nil {
		db.t.Fatalf("unexpected query: %s", sql)
	}
	return db.queryFn(ctx, sql, args...)
}
func (db *fakeDB) QueryRow(ctx context.Context, sql string, args ...any) pgx.Row {
	if db.queryRowFn == nil {
		db.t.Fatalf("unexpected query row: %s", sql)
	}
	return db.queryRowFn(ctx, sql, args...)
}
func (db *fakeDB) Exec(ctx context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	if db.execFn == nil {
		db.t.Fatal("unexpected exec")
	}
	return db.execFn(ctx, sql, args...)
}
func (db *fakeDB) Begin(ctx context.Context) (pgx.Tx, error) {
	if db.beginFn == nil {
		db.t.Fatal("unexpected begin")
	}
	return db.beginFn(ctx)
}

// fakeRowSet models a pgx.Rows cursor without a database.
type fakeRowSet struct {
	pgx.Rows
	rows   []pgx.Row
	index  int
	err    error
	closed int
}

func (r *fakeRowSet) Next() bool {
	if r.index >= len(r.rows) {
		return false
	}
	r.index++
	return true
}
func (r *fakeRowSet) Scan(dest ...any) error { return r.rows[r.index-1].Scan(dest...) }
func (r *fakeRowSet) Close()                 { r.closed++ }
func (r *fakeRowSet) Err() error             { return r.err }

func updateTag(n int64) pgconn.CommandTag { return pgconn.NewCommandTag(fmt.Sprintf("UPDATE %d", n)) }
func deleteTag(n int64) pgconn.CommandTag { return pgconn.NewCommandTag(fmt.Sprintf("DELETE %d", n)) }

func hostRow(id, fingerprint string, protocol int32, at time.Time) pgx.Row {
	return syntheticRow{fill: func(dest ...any) {
		*dest[0].(*string) = id
		*dest[1].(*string) = fingerprint
		*dest[2].(*int32) = protocol
		*dest[3].(*time.Time) = at
		*dest[4].(*time.Time) = at.Add(time.Second)
	}}
}

func deviceRow(id, fingerprint string, authHash []byte, at time.Time) pgx.Row {
	return syntheticRow{fill: func(dest ...any) {
		*dest[0].(*string) = id
		*dest[1].(*string) = fingerprint
		*dest[2].(*[]byte) = authHash
		*dest[3].(*time.Time) = at
		*dest[4].(*time.Time) = at.Add(time.Second)
	}}
}

func revocationRow(device string, revokedAt time.Time, generation uint64) pgx.Row {
	return syntheticRow{fill: func(dest ...any) {
		*dest[0].(*string) = device
		*dest[1].(*time.Time) = revokedAt
		*dest[2].(*uint64) = generation
	}}
}

func pairingTokenRow(hash []byte, host string, at time.Time) pgx.Row {
	return syntheticRow{fill: func(dest ...any) {
		*dest[0].(*[]byte) = hash
		*dest[1].(*string) = host
		*dest[2].(*time.Time) = at
		*dest[3].(**time.Time) = nil
		*dest[4].(*int) = 2
		*dest[5].(**time.Time) = nil
		*dest[6].(*time.Time) = at
		*dest[7].(*time.Time) = at
	}}
}

func pairingOperationRow(claim string, at time.Time) pgx.Row {
	return syntheticRow{fill: func(dest ...any) {
		*dest[0].(*string) = claim
		*dest[1].(*[]byte) = []byte{1}
		*dest[2].(*string) = "host"
		*dest[3].(*string) = "device"
		*dest[4].(*[]byte) = []byte{2}
		*dest[5].(*[]byte) = []byte{3}
		*dest[6].(*time.Time) = at
		*dest[7].(*[]byte) = []byte{4}
		*dest[8].(*[]byte) = []byte{5}
		*dest[9].(*string) = "request"
		*dest[10].(*uint64) = 4
		*dest[11].(*[]byte) = []byte{6}
		*dest[12].(*bool) = true
	}}
}

func TestHostStoreOperationsModelDatabaseOutcomes(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("ensure host creates then reads", func(t *testing.T) {
		executed, read := 0, 0
		db := &fakeDB{t: t,
			execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
				executed++
				return pgconn.NewCommandTag("INSERT 0 1"), nil
			},
			queryRowFn: func(context.Context, string, ...any) pgx.Row {
				read++
				return hostRow("host", "fingerprint", 2, at)
			},
		}
		host, err := newStore(db).EnsureHost(context.Background(), "host")
		if err != nil || host.HostID != "host" || host.ProtocolVersion != 2 || executed != 1 || read != 1 {
			t.Fatalf("ensure host = %+v err=%v executed=%d read=%d", host, err, executed, read)
		}
	})

	t.Run("ensure host reports creation failure before reading", func(t *testing.T) {
		db := &fakeDB{t: t,
			execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
				return pgconn.CommandTag{}, errStoreUnit
			},
			queryRowFn: func(context.Context, string, ...any) pgx.Row { t.Fatal("read after failed create"); return nil },
		}
		if _, err := newStore(db).EnsureHost(context.Background(), "host"); !errors.Is(err, errStoreUnit) {
			t.Fatalf("creation failure = %v", err)
		}
	})

	t.Run("host metadata treats zero rows as a missing host", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			rows    int64
			execErr error
			wantErr error
		}{
			{"updated", 1, nil, nil},
			{"missing", 0, nil, ErrNotFound},
			{"storage failure", 0, errStoreUnit, errStoreUnit},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
					return updateTag(tc.rows), tc.execErr
				}}
				if err := newStore(db).UpdateHostMetadata(context.Background(), "host", "fingerprint", 3, at); !errors.Is(err, tc.wantErr) || (tc.wantErr == nil && err != nil) {
					t.Fatalf("metadata error = %v", err)
				}
			})
		}
	})

	t.Run("host authorization key keeps the first registration", func(t *testing.T) {
		key := make([]byte, 32)
		key[0] = 7
		for _, tc := range []struct {
			name      string
			rows      int64
			execErr   error
			existing  []byte
			readErr   error
			wantErr   error
			wantReads int
		}{
			{"first registration", 1, nil, nil, nil, nil, 0},
			{"existing key", 0, nil, key, nil, nil, 1},
			{"host missing", 0, nil, nil, pgx.ErrNoRows, ErrNotFound, 1},
			{"read failure", 0, nil, nil, errStoreUnit, errStoreUnit, 1},
			{"write failure", 0, errStoreUnit, nil, nil, errStoreUnit, 0},
		} {
			t.Run(tc.name, func(t *testing.T) {
				reads := 0
				db := &fakeDB{t: t,
					execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
						return updateTag(tc.rows), tc.execErr
					},
					queryRowFn: func(context.Context, string, ...any) pgx.Row {
						reads++
						if tc.readErr != nil {
							return syntheticRow{err: tc.readErr}
						}
						return syntheticRow{fill: func(dest ...any) { *dest[0].(*[]byte) = tc.existing }}
					},
				}
				if err := newStore(db).SetHostAuthorizationKey(context.Background(), "host", key); !errors.Is(err, tc.wantErr) || (tc.wantErr == nil && err != nil) || reads != tc.wantReads {
					t.Fatalf("authorization key error=%v reads=%d", err, reads)
				}
			})
		}
	})

	t.Run("host authorization key requires a registered key", func(t *testing.T) {
		key := make([]byte, 32)
		for _, tc := range []struct {
			name    string
			key     []byte
			wantErr error
		}{{"registered", key, nil}, {"unregistered", nil, ErrNotFound}, {"truncated", key[:31], ErrNotFound}, {"missing row", nil, ErrNotFound}, {"storage failure", nil, errStoreUnit}} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
					switch {
					case tc.name == "missing row":
						return syntheticRow{err: pgx.ErrNoRows}
					case tc.name == "storage failure":
						return syntheticRow{err: errStoreUnit}
					default:
						return syntheticRow{fill: func(dest ...any) { *dest[0].(*[]byte) = tc.key }}
					}
				}}
				got, err := newStore(db).HostAuthorizationKey(context.Background(), "host")
				if tc.wantErr == nil && (err != nil || len(got) != 32) {
					t.Fatalf("registered key = %x err=%v", got, err)
				}
				if tc.wantErr != nil && !errors.Is(err, tc.wantErr) {
					t.Fatalf("error = %v", err)
				}
			})
		}
	})
}

func TestHostListingAndRevocationReadsModelDatabaseOutcomes(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("authorized hosts preserve order and close the cursor", func(t *testing.T) {
		rows := &fakeRowSet{rows: []pgx.Row{hostRow("h1", "", 1, at), hostRow("h2", "", 2, at)}}
		db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }}
		hosts, err := newStore(db).ListAuthorizedHosts(context.Background(), "device")
		if err != nil || len(hosts) != 2 || hosts[0].HostID != "h1" || hosts[1].HostID != "h2" || rows.closed != 1 {
			t.Fatalf("hosts=%v err=%v closed=%d", hosts, err, rows.closed)
		}
	})

	t.Run("authorized hosts stop on scan and cursor failures", func(t *testing.T) {
		for _, tc := range []struct {
			name string
			rows *fakeRowSet
		}{
			{"scan failure", &fakeRowSet{rows: []pgx.Row{syntheticRow{err: errStoreUnit}}}},
			{"cursor failure", &fakeRowSet{err: errStoreUnit}},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return tc.rows, nil }}
				if _, err := newStore(db).ListAuthorizedHosts(context.Background(), "device"); !errors.Is(err, errStoreUnit) || tc.rows.closed != 1 {
					t.Fatalf("list error=%v closed=%d", err, tc.rows.closed)
				}
			})
		}
	})

	t.Run("device reads and grant lookup", func(t *testing.T) {
		authHash := []byte{9}
		db := &fakeDB{t: t, queryRowFn: func(_ context.Context, sql string, _ ...any) pgx.Row {
			switch {
			case strings.Contains(sql, "SELECT EXISTS"):
				return syntheticRow{fill: func(dest ...any) { *dest[0].(*bool) = true }}
			case strings.Contains(sql, "ORDER BY c.created_at"):
				return deviceRow("device", "fingerprint", authHash, at)
			default:
				return deviceRow("device", "fingerprint", authHash, at)
			}
		}}
		s := newStore(db)
		active, err := s.ActiveDeviceByAuthHash(context.Background(), authHash)
		if err != nil || active.DeviceID != "device" {
			t.Fatalf("active device = %+v err=%v", active, err)
		}
		byID, err := s.DeviceByID(context.Background(), "device")
		if err != nil || byID.DeviceID != "device" {
			t.Fatalf("device by id = %+v err=%v", byID, err)
		}
		granted, err := s.HasActiveGrant(context.Background(), "host", "device")
		if err != nil || !granted {
			t.Fatalf("grant lookup = %v err=%v", granted, err)
		}
	})

	t.Run("missing device rows are not found", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return syntheticRow{err: pgx.ErrNoRows} }}
		s := newStore(db)
		if _, err := s.ActiveDeviceByAuthHash(context.Background(), []byte{1}); !errors.Is(err, ErrNotFound) {
			t.Fatalf("active device error = %v", err)
		}
		if _, err := s.DeviceByID(context.Background(), "device"); !errors.Is(err, ErrNotFound) {
			t.Fatalf("device error = %v", err)
		}
		if _, err := s.HasActiveGrant(context.Background(), "host", "device"); !errors.Is(err, pgx.ErrNoRows) {
			t.Fatalf("grant error = %v", err)
		}
	})

	t.Run("pending revocations report only unreplayed rows", func(t *testing.T) {
		rows := &fakeRowSet{rows: []pgx.Row{revocationRow("d1", at, 2), revocationRow("d2", at, 3)}}
		db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }}
		revs, err := newStore(db).PendingRevocations(context.Background(), "host")
		if err != nil || len(revs) != 2 || revs[0].HostID != "host" || revs[1].GrantGeneration != 3 || rows.closed != 1 {
			t.Fatalf("revocations=%v err=%v", revs, err)
		}
	})

	t.Run("pending revocations fail closed", func(t *testing.T) {
		for _, tc := range []struct {
			name string
			rows *fakeRowSet
		}{
			{"scan failure", &fakeRowSet{rows: []pgx.Row{syntheticRow{err: errStoreUnit}}}},
			{"cursor failure", &fakeRowSet{err: errStoreUnit}},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return tc.rows, nil }}
				if _, err := newStore(db).PendingRevocations(context.Background(), "host"); !errors.Is(err, errStoreUnit) || tc.rows.closed != 1 {
					t.Fatalf("error=%v closed=%d", err, tc.rows.closed)
				}
			})
		}
	})

	t.Run("acknowledged revocations are recorded per device", func(t *testing.T) {
		calls := 0
		db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
			calls++
			if calls == 2 {
				return pgconn.CommandTag{}, errStoreUnit
			}
			return updateTag(1), nil
		}}
		err := newStore(db).MarkRevocationsReplayed(context.Background(), "host", []string{"d1", "d2"}, at, at, map[string]uint64{"d1": 1, "d2": 2})
		if !errors.Is(err, errStoreUnit) || calls != 2 {
			t.Fatalf("replay error=%v calls=%d", err, calls)
		}
	})

	t.Run("active grant count", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return syntheticRow{fill: func(dest ...any) { *dest[0].(*int) = 4 }}
		}}
		count, err := newStore(db).CountActiveGrants(context.Background(), "device")
		if err != nil || count != 4 {
			t.Fatalf("count=%d err=%v", count, err)
		}
	})
}
