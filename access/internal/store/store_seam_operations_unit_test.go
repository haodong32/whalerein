package store

import (
	"context"
	"errors"
	"strings"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	whalereinv1 "github.com/whalerein/whalerein/proto/v1/bindings/go"
)

func TestPairingTokenOperationsModelDatabaseOutcomes(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	hash := []byte{1, 2, 3}

	t.Run("create invitation reports uniqueness conflicts", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			execErr error
			wantErr error
		}{
			{"stored", nil, nil},
			{"duplicate invitation", &pgconn.PgError{Code: "23505"}, ErrConflict},
			{"storage failure", errStoreUnit, errStoreUnit},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
					return pgconn.NewCommandTag("INSERT 0 1"), tc.execErr
				}}
				if err := newStore(db).CreatePairingToken(context.Background(), hash, "host", at, at); !errors.Is(err, tc.wantErr) || (tc.wantErr == nil && err != nil) {
					t.Fatalf("create invitation error = %v", err)
				}
			})
		}
	})

	t.Run("read invitation preserves its one-time state", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return pairingTokenRow(hash, "host", at) }}
		token, err := newStore(db).PairingTokenByHash(context.Background(), hash)
		if err != nil || token.HostID != "host" || token.FailureCount != 2 || token.UsedAt != nil {
			t.Fatalf("token = %+v err=%v", token, err)
		}
	})

	t.Run("missing invitation is not found", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			row     pgx.Row
			wantErr error
		}{
			{"absent", syntheticRow{err: pgx.ErrNoRows}, ErrNotFound},
			{"storage failure", syntheticRow{err: errStoreUnit}, errStoreUnit},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return tc.row }}
				if _, err := newStore(db).PairingTokenByHash(context.Background(), hash); !errors.Is(err, tc.wantErr) {
					t.Fatalf("read invitation error = %v", err)
				}
			})
		}
	})

	t.Run("failed attempt returns the remaining budget", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
			return syntheticRow{fill: func(dest ...any) { *dest[0].(*int) = 3 }}
		}}
		count, err := newStore(db).IncrementPairingFailure(context.Background(), hash, at)
		if err != nil || count != 3 {
			t.Fatalf("failure count = %d err=%v", count, err)
		}
	})

	t.Run("consuming an invitation succeeds exactly once", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			rows    int64
			execErr error
			want    bool
		}{
			{"first use", 1, nil, true},
			{"already used", 0, nil, false},
			{"storage failure", 0, errStoreUnit, false},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
					return updateTag(tc.rows), tc.execErr
				}}
				used, err := newStore(db).MarkPairingTokenUsed(context.Background(), hash, at)
				if tc.execErr != nil {
					if used || !errors.Is(err, tc.execErr) {
						t.Fatalf("used=%v err=%v", used, err)
					}
					return
				}
				if used != tc.want || err != nil {
					t.Fatalf("used=%v err=%v", used, err)
				}
			})
		}
	})
}

func TestDeleteExpiredPairingTokensReconcilesBeforeDeleting(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	build := func(t *testing.T, mode string) (*fakeDB, *[]string) {
		t.Helper()
		steps := &[]string{}
		db := &fakeDB{t: t,
			queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
				*steps = append(*steps, "reconcile")
				if mode == "reconcile-failure" {
					return nil, errStoreUnit
				}
				return &fakeRowSet{}, nil
			},
			execFn: func(_ context.Context, sql string, _ ...any) (pgconn.CommandTag, error) {
				switch {
				case strings.Contains(sql, "pairing_operations"):
					*steps = append(*steps, "operations")
					if mode == "operations-failure" {
						return pgconn.CommandTag{}, errStoreUnit
					}
					return deleteTag(2), nil
				default:
					*steps = append(*steps, "tokens")
					if mode == "tokens-failure" {
						return pgconn.CommandTag{}, errStoreUnit
					}
					return deleteTag(4), nil
				}
			},
		}
		return db, steps
	}

	t.Run("reconciles first and removes expired rows", func(t *testing.T) {
		db, steps := build(t, "ok")
		deleted, err := newStore(db).DeleteExpiredPairingTokens(context.Background(), at)
		// operations=2 and tokens=4 are both reported so the cleanup drain sees
		// the complete bounded work.
		if err != nil || deleted != 6 || len(*steps) != 3 || (*steps)[0] != "reconcile" || (*steps)[1] != "operations" || (*steps)[2] != "tokens" {
			t.Fatalf("deleted=%d err=%v steps=%v", deleted, err, *steps)
		}
	})

	t.Run("reconciliation failure stops all deletions", func(t *testing.T) {
		db, steps := build(t, "reconcile-failure")
		if _, err := newStore(db).DeleteExpiredPairingTokens(context.Background(), at); !errors.Is(err, errStoreUnit) || len(*steps) != 1 {
			t.Fatalf("err=%v steps=%v", err, *steps)
		}
	})

	t.Run("operation deletion failure stops token deletion", func(t *testing.T) {
		db, steps := build(t, "operations-failure")
		if _, err := newStore(db).DeleteExpiredPairingTokens(context.Background(), at); !errors.Is(err, errStoreUnit) || len(*steps) != 2 || (*steps)[1] != "operations" {
			t.Fatalf("err=%v steps=%v", err, *steps)
		}
	})

	t.Run("token deletion failure is reported", func(t *testing.T) {
		db, steps := build(t, "tokens-failure")
		if _, err := newStore(db).DeleteExpiredPairingTokens(context.Background(), at); !errors.Is(err, errStoreUnit) || len(*steps) != 3 {
			t.Fatalf("err=%v steps=%v", err, *steps)
		}
	})
}

func TestRequestStoreOperationsModelDatabaseOutcomes(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("request read maps rows and absence", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(_ context.Context, _ string, _ ...any) pgx.Row {
			return requestFixtureRow([]byte{1})
		}}
		req, err := newStore(db).GetRequest(context.Background(), "request")
		if err != nil || req.RequestID != "request" || req.HostID != "host" || !req.CreatedAt.Equal(time.Unix(123, 0).UTC()) {
			t.Fatalf("request = %+v err=%v", req, err)
		}
	})

	t.Run("request read fails closed", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			row     pgx.Row
			wantErr error
		}{{"absent", syntheticRow{err: pgx.ErrNoRows}, ErrNotFound}, {"storage failure", syntheticRow{err: errStoreUnit}, errStoreUnit}} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return tc.row }}
				if _, err := newStore(db).GetRequest(context.Background(), "request"); !errors.Is(err, tc.wantErr) {
					t.Fatalf("get request error = %v", err)
				}
			})
		}
	})

	t.Run("due claiming leases bounded rows", func(t *testing.T) {
		rows := &fakeRowSet{rows: []pgx.Row{requestFixtureRow([]byte{1}), requestFixtureRow([]byte{2})}}
		db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return rows, nil }}
		claimed, err := newStore(db).ClaimDueRequests(context.Background(), at, 5, time.Minute)
		if err != nil || len(claimed) != 2 || claimed[0].RequestID != "request" || rows.closed != 1 {
			t.Fatalf("claimed=%v err=%v closed=%d", claimed, err, rows.closed)
		}
	})

	t.Run("due claiming fails closed", func(t *testing.T) {
		for _, tc := range []struct {
			name     string
			rows     *fakeRowSet
			queryErr error
		}{
			{"query failure", nil, errStoreUnit},
			{"scan failure", &fakeRowSet{rows: []pgx.Row{syntheticRow{err: errStoreUnit}}}, nil},
			{"cursor failure", &fakeRowSet{err: errStoreUnit}, nil},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
					if tc.queryErr != nil {
						return nil, tc.queryErr
					}
					return tc.rows, nil
				}}
				if _, err := newStore(db).ClaimDueRequests(context.Background(), at, 5, time.Minute); !errors.Is(err, errStoreUnit) {
					t.Fatalf("claim error = %v", err)
				}
			})
		}
	})

	t.Run("publish lifecycle updates reported row counts", func(t *testing.T) {
		for _, tc := range []struct {
			name string
			call func(*Store) error
		}{
			{"requeue", func(s *Store) error {
				n, err := s.RequeueDuePublished(context.Background(), at, 3)
				if err != nil || n != 2 {
					return errors.Join(err, errors.New("unexpected requeue count"))
				}
				return nil
			}},
			{"published", func(s *Store) error { return s.MarkPublished(context.Background(), "request", at, time.Minute) }},
			{"publish failure", func(s *Store) error { return s.MarkPublishFailure(context.Background(), "request", at, time.Minute, 3) }},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
					return updateTag(2), nil
				}}
				if err := tc.call(newStore(db)); err != nil {
					t.Fatalf("publish lifecycle: %v", err)
				}
			})
		}
	})

	t.Run("publish lifecycle reports storage failures", func(t *testing.T) {
		for _, tc := range []struct {
			name string
			call func(*Store) error
		}{
			{"requeue", func(s *Store) error { _, err := s.RequeueDuePublished(context.Background(), at, 3); return err }},
			{"published", func(s *Store) error { return s.MarkPublished(context.Background(), "request", at, time.Minute) }},
			{"publish failure", func(s *Store) error { return s.MarkPublishFailure(context.Background(), "request", at, time.Minute, 3) }},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
					return pgconn.CommandTag{}, errStoreUnit
				}}
				if err := tc.call(newStore(db)); !errors.Is(err, errStoreUnit) {
					t.Fatalf("publish lifecycle error = %v", err)
				}
			})
		}
	})

	t.Run("body cleanup reconciles first", func(t *testing.T) {
		steps := []string{}
		db := &fakeDB{t: t,
			queryFn: func(context.Context, string, ...any) (pgx.Rows, error) {
				steps = append(steps, "reconcile")
				return &fakeRowSet{}, nil
			},
			execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
				steps = append(steps, "clear")
				return updateTag(5), nil
			},
		}
		cleared, err := newStore(db).ClearExpiredBodies(context.Background(), at)
		if err != nil || cleared != 5 || len(steps) != 2 || steps[0] != "reconcile" {
			t.Fatalf("cleared=%d err=%v steps=%v", cleared, err, steps)
		}
	})

	t.Run("body cleanup stops when reconciliation fails", func(t *testing.T) {
		executed := false
		db := &fakeDB{t: t,
			queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return nil, errStoreUnit },
			execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
				executed = true
				return updateTag(1), nil
			},
		}
		if _, err := newStore(db).ClearExpiredBodies(context.Background(), at); !errors.Is(err, errStoreUnit) || executed {
			t.Fatalf("cleanup err=%v executed=%v", err, executed)
		}
	})

	t.Run("table helpers read and delete bounded rows", func(t *testing.T) {
		t.Run("count requests", func(t *testing.T) {
			db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row {
				return syntheticRow{fill: func(dest ...any) { *dest[0].(*int) = 6 }}
			}}
			count, err := newStore(db).CountRequests(context.Background())
			if err != nil || count != 6 {
				t.Fatalf("count=%d err=%v", count, err)
			}
		})
		t.Run("delete expired queries", func(t *testing.T) {
			db := &fakeDB{t: t, execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) { return deleteTag(7), nil }}
			deleted, err := newStore(db).DeleteExpiredQueries(context.Background(), at)
			if err != nil || deleted != 7 {
				t.Fatalf("deleted=%d err=%v", deleted, err)
			}
		})
	})
}

func TestGenerationAndOperationReadsModelDatabaseOutcomes(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("device generation", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			row     pgx.Row
			want    int64
			wantErr error
		}{
			{"revoked once", generationRow{values: []int64{2}}, 2, nil},
			{"storage failure", generationRow{err: errStoreUnit}, 0, errStoreUnit},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return tc.row }}
				got, err := newStore(db).DeviceGeneration(context.Background(), "device")
				if got != tc.want || !errors.Is(err, tc.wantErr) {
					t.Fatalf("generation=%d err=%v", got, err)
				}
			})
		}
	})

	t.Run("pairing generations require an invitation snapshot", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			row     pgx.Row
			wantErr error
		}{
			{"snapshot", generationRow{values: []int64{2, 3}}, nil},
			{"missing invitation", generationRow{err: pgx.ErrNoRows}, ErrNotFound},
			{"storage failure", generationRow{err: errStoreUnit}, errStoreUnit},
		} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return tc.row }}
				device, host, err := newStore(db).PairingGenerations(context.Background(), []byte{1}, "device")
				if !errors.Is(err, tc.wantErr) {
					t.Fatalf("generations error = %v", err)
				}
				if tc.wantErr == nil && (device != 2 || host != 3) {
					t.Fatalf("generations = %d/%d", device, host)
				}
			})
		}
	})

	t.Run("operation progress maps the committed grant", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return pairingOperationRow("claim", at) }}
		op, err := newStore(db).PairingOperation(context.Background(), "claim")
		if err != nil || op.ClaimID != "claim" || op.GrantGeneration != 4 || !op.Active {
			t.Fatalf("operation = %+v err=%v", op, err)
		}
	})

	t.Run("operation progress fails closed", func(t *testing.T) {
		for _, tc := range []struct {
			name    string
			row     pgx.Row
			wantErr error
		}{{"missing", syntheticRow{err: pgx.ErrNoRows}, ErrNotFound}, {"storage failure", syntheticRow{err: errStoreUnit}, errStoreUnit}} {
			t.Run(tc.name, func(t *testing.T) {
				db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return tc.row }}
				if _, err := newStore(db).PairingOperation(context.Background(), "claim"); !errors.Is(err, tc.wantErr) {
					t.Fatalf("operation error = %v", err)
				}
			})
		}
	})
}

// Every transaction-backed public wrapper must surface an unavailable unit of
// work as an error instead of reporting success or a committed outcome.
func TestTransactionWrappersFailClosedWhenDatabaseCannotBegin(t *testing.T) {
	begin := func(context.Context) (pgx.Tx, error) { return nil, errStoreUnit }
	s := newStore(&fakeDB{t: t, beginFn: begin})

	if err := s.SavePairingOperation(context.Background(), PairingOperation{}); !errors.Is(err, errStoreUnit) {
		t.Fatalf("save operation = %v", err)
	}
	if fenced, err := s.PairingFenced(context.Background(), "claim", "host", "device"); fenced || !errors.Is(err, errStoreUnit) {
		t.Fatalf("fenced=%v err=%v", fenced, err)
	}
	if outcome, err := s.PairingOutcome(context.Background(), "claim", "device"); outcome.Active || !errors.Is(err, errStoreUnit) {
		t.Fatalf("outcome=%+v err=%v", outcome, err)
	}
	if confirmed, err := s.ConfirmPairingHost(context.Background(), "claim", "host", "device", "request", time.Time{}); confirmed || !errors.Is(err, errStoreUnit) {
		t.Fatalf("confirmed=%v err=%v", confirmed, err)
	}
	if err := s.CreatePairedDevice(context.Background(), "host", "device", "fingerprint", []byte{1}, time.Time{}); !errors.Is(err, errStoreUnit) {
		t.Fatalf("register device = %v", err)
	}
	if err := s.RevokeGrant(context.Background(), "host", "device", time.Time{}); !errors.Is(err, errStoreUnit) {
		t.Fatalf("revoke grant = %v", err)
	}
	if _, _, err := s.RevokeDeviceGrants(context.Background(), "device", time.Time{}); !errors.Is(err, errStoreUnit) {
		t.Fatalf("revoke device = %v", err)
	}
	if err := s.Migrate(context.Background()); !errors.Is(err, errStoreUnit) {
		t.Fatalf("migrate = %v", err)
	}
}

func TestReconciliationWrapperReadsEligibleOutcomes(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()
	db := &fakeDB{t: t, queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return &fakeRowSet{}, nil }}
	if err := newStore(db).reconcilePairingOutcomes(context.Background(), at, true); err != nil {
		t.Fatalf("reconcile = %v", err)
	}
}

func TestStoreLifecycleUsesConfiguredDatabase(t *testing.T) {
	db := &fakeDB{t: t}
	s := newStore(db)
	if err := s.Ping(context.Background()); err != nil {
		t.Fatalf("ping = %v", err)
	}
	if s.Pool() != nil {
		t.Fatal("scripted database must not expose a PostgreSQL pool")
	}
	s.Close()
	if db.closed != 1 {
		t.Fatalf("close count = %d", db.closed)
	}

	db.pingErr = errStoreUnit
	if err := s.Ping(context.Background()); !errors.Is(err, errStoreUnit) {
		t.Fatalf("ping failure = %v", err)
	}

	empty := NewWithPool(nil)
	if empty == nil || empty.Pool() != nil {
		t.Fatal("nil pool must produce a usable empty store")
	}
	empty.Close()
	if err := empty.Ping(context.Background()); err == nil {
		t.Fatal("empty store reported a healthy database")
	}
}

func TestStoreBoundaryReadsAndWritesSurfaceStorageFailures(t *testing.T) {
	at := time.Unix(1_700_000_000, 0).UTC()

	t.Run("host generation", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return generationRow{values: []int64{5}} }}
		generation, err := newStore(db).HostGeneration(context.Background(), "host", "device")
		if err != nil || generation != 5 {
			t.Fatalf("host generation=%d err=%v", generation, err)
		}
	})

	t.Run("create request uses the store seam", func(t *testing.T) {
		db := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return requestFixtureRow([]byte{1}) }}
		result, err := newStore(db).CreateRequest(context.Background(), Request{RequestID: "request", HostID: "host", DeviceID: "device", Status: whalereinv1.OnlineStatus_received, ClientTransportVersion: 2})
		if err != nil || !result.Created {
			t.Fatalf("create request created=%v err=%v", result.Created, err)
		}
	})

	t.Run("count helpers report storage failures", func(t *testing.T) {
		rowDB := &fakeDB{t: t, queryRowFn: func(context.Context, string, ...any) pgx.Row { return syntheticRow{err: errStoreUnit} }}
		if _, err := newStore(rowDB).CountActiveGrants(context.Background(), "device"); !errors.Is(err, errStoreUnit) {
			t.Fatalf("grant count error = %v", err)
		}
		if _, err := newStore(rowDB).CountRequests(context.Background()); !errors.Is(err, errStoreUnit) {
			t.Fatalf("request count error = %v", err)
		}
		if _, err := newStore(rowDB).IncrementPairingFailure(context.Background(), []byte{1}, at); !errors.Is(err, errStoreUnit) {
			t.Fatalf("failure count error = %v", err)
		}
	})

	t.Run("delete and cleanup helpers report storage failures", func(t *testing.T) {
		execDB := &fakeDB{t: t,
			queryFn: func(context.Context, string, ...any) (pgx.Rows, error) { return &fakeRowSet{}, nil },
			execFn: func(context.Context, string, ...any) (pgconn.CommandTag, error) {
				return pgconn.CommandTag{}, errStoreUnit
			},
		}
		if _, err := newStore(execDB).DeleteExpiredQueries(context.Background(), at); !errors.Is(err, errStoreUnit) {
			t.Fatalf("delete error = %v", err)
		}
		if _, err := newStore(execDB).DeleteExpiredPairingTokens(context.Background(), at); !errors.Is(err, errStoreUnit) {
			t.Fatalf("pairing retention error = %v", err)
		}
	})
}
