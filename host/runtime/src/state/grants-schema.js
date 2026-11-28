// Host-owned device trust schema. Additive to the plugin state store so an
// existing journal upgrades in place; every table is created with IF NOT
// EXISTS and no legacy row is rewritten. The store keeps one SQLite writer
// and the same WAL/FULL durability settings for these tables.

/**
 * Apply the device-trust tables to an open Host SQLite database. Idempotent.
 * @param {import('node:sqlite').DatabaseSync} db
 */
export function migrateGrantsSchema(db) {
  db.exec(`
    -- One row per issued grant. Retired rows are fences: they are never
    -- deleted, so a retry can observe the original decision and a revoked
    -- grant can never be revived.
    CREATE TABLE IF NOT EXISTS device_grants (
      grant_id TEXT PRIMARY KEY,
      host_key_fingerprint TEXT NOT NULL,
      device_public_key BLOB NOT NULL CHECK (length(device_public_key) = 32),
      device_id TEXT NOT NULL,
      device_label TEXT NOT NULL DEFAULT '',
      use_rights TEXT NOT NULL,
      grantable_rights TEXT NOT NULL,
      add_device INTEGER NOT NULL DEFAULT 0 CHECK (add_device IN (0, 1)),
      approver_kind TEXT NOT NULL CHECK (approver_kind IN ('owner', 'sponsor', 'recovery')),
      sponsor_grant_id TEXT,
      recovery_verifier_revision INTEGER,
      revision INTEGER NOT NULL CHECK (revision >= 1),
      state TEXT NOT NULL CHECK (state IN ('active', 'revoked')),
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL,
      revoked_at INTEGER,
      CHECK (approver_kind <> 'sponsor' OR sponsor_grant_id IS NOT NULL),
      CHECK (approver_kind <> 'recovery' OR recovery_verifier_revision IS NOT NULL)
    );
    -- At most one ACTIVE grant for a (Host key, device key) pair. Revoked
    -- rows stay outside the index and remain available as revocation fences.
    CREATE UNIQUE INDEX IF NOT EXISTS device_grants_one_active
      ON device_grants (host_key_fingerprint, device_public_key)
      WHERE state = 'active';
    CREATE INDEX IF NOT EXISTS device_grants_device
      ON device_grants (host_key_fingerprint, device_public_key, state);
    CREATE INDEX IF NOT EXISTS device_grants_sponsor
      ON device_grants (sponsor_grant_id, state);
    CREATE INDEX IF NOT EXISTS device_grants_state
      ON device_grants (state, updated_at);

    -- One row per pending candidate and, after the commit, its durable
    -- decision. A late retry compares its request digest against this row:
    -- matching retries observe the original outcome, different requests
    -- conflict instead of merging or replacing rights.
    CREATE TABLE IF NOT EXISTS enrollment_candidates (
      candidate_id TEXT PRIMARY KEY,
      method TEXT NOT NULL CHECK (method IN ('local', 'sponsor', 'recovery')),
      host_key_fingerprint TEXT NOT NULL,
      candidate_public_key BLOB NOT NULL CHECK (length(candidate_public_key) = 32),
      device_id TEXT NOT NULL,
      device_label TEXT NOT NULL DEFAULT '',
      use_rights TEXT NOT NULL,
      grantable_rights TEXT NOT NULL,
      add_device INTEGER NOT NULL DEFAULT 0 CHECK (add_device IN (0, 1)),
      sponsor_grant_id TEXT,
      sponsor_revision INTEGER,
      recovery_verifier_revision INTEGER,
      request_digest BLOB NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('pending', 'approved', 'rejected', 'fenced')),
      grant_id TEXT,
      decided_at INTEGER,
      expires_at INTEGER NOT NULL,
      created_at INTEGER NOT NULL,
      CHECK (method <> 'sponsor' OR sponsor_grant_id IS NOT NULL),
      CHECK (method <> 'recovery' OR recovery_verifier_revision IS NOT NULL)
    );
    CREATE INDEX IF NOT EXISTS enrollment_candidates_pending
      ON enrollment_candidates (state, expires_at);
    CREATE INDEX IF NOT EXISTS enrollment_candidates_sponsor
      ON enrollment_candidates (sponsor_grant_id, state);
    CREATE INDEX IF NOT EXISTS enrollment_candidates_key
      ON enrollment_candidates (host_key_fingerprint, candidate_public_key, state);

    -- Recovery verifier registrations. AUTOINCREMENT makes the revision
    -- monotonic and never reusable, including retirement followed by a
    -- re-registration of identical key bytes.
    CREATE TABLE IF NOT EXISTS recovery_verifiers (
      revision INTEGER PRIMARY KEY AUTOINCREMENT,
      host_key_fingerprint TEXT NOT NULL,
      public_key BLOB NOT NULL CHECK (length(public_key) = 32),
      allowed_rights TEXT NOT NULL,
      active INTEGER NOT NULL DEFAULT 1 CHECK (active IN (0, 1)),
      created_at INTEGER NOT NULL,
      retired_at INTEGER
    );
    CREATE UNIQUE INDEX IF NOT EXISTS recovery_verifiers_one_active
      ON recovery_verifiers (host_key_fingerprint)
      WHERE active = 1;

    -- Host-issued, single-use recovery challenge. The nonce, revision,
    -- applicant key, exact rights and expiry are all persisted so a restart
    -- cannot resurrect a consumed or retired challenge.
    CREATE TABLE IF NOT EXISTS recovery_challenges (
      challenge_id TEXT PRIMARY KEY,
      host_key_fingerprint TEXT NOT NULL,
      verifier_revision INTEGER NOT NULL,
      nonce TEXT NOT NULL UNIQUE,
      candidate_public_key BLOB NOT NULL CHECK (length(candidate_public_key) = 32),
      use_rights TEXT NOT NULL,
      expires_at INTEGER NOT NULL,
      consumed INTEGER NOT NULL DEFAULT 0 CHECK (consumed IN (0, 1)),
      consumed_at INTEGER,
      grant_id TEXT,
      created_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS recovery_challenges_revision
      ON recovery_challenges (verifier_revision, consumed, expires_at);

    -- Host-wide command reservation. The primary key is the command ID alone,
    -- so duplicate suppression survives a grant change or a re-enrollment;
    -- the intent binding includes the originating key and original deadline.
    CREATE TABLE IF NOT EXISTS command_journal (
      command_id TEXT PRIMARY KEY,
      intent_digest BLOB NOT NULL CHECK (length(intent_digest) = 32),
      device_public_key BLOB NOT NULL CHECK (length(device_public_key) = 32),
      grant_id TEXT NOT NULL,
      deadline_at INTEGER NOT NULL,
      state TEXT NOT NULL CHECK (state IN ('reserved', 'succeeded', 'failed', 'unknown')),
      result_payload BLOB,
      error_code TEXT,
      created_at INTEGER NOT NULL,
      updated_at INTEGER NOT NULL
    );
    CREATE INDEX IF NOT EXISTS command_journal_grant
      ON command_journal (grant_id, created_at);
    CREATE INDEX IF NOT EXISTS command_journal_state
      ON command_journal (state, updated_at);
  `)
}