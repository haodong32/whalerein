// Plugin-owned SQLite state. This is the only datastore in the plugin: host
// public identity reference, peer public keys, applied revocations, command
// dedupe/status/results, per-session sequence bases, and the bounded ciphertext
// outbox. Message content, private keys, and a second event log never enter it.

import { mkdirSync } from 'node:fs'
import { dirname } from 'node:path'
import { DatabaseSync } from 'node:sqlite'
import { PluginError } from '../errors.js'

const SCHEMA_VERSION = 4

function blob(value) {
  if (value === null || value === undefined) return null
  return Buffer.from(value)
}

function rowCommand(row) {
  if (!row) return null
  return {
    deviceId: row.device_id,
    commandId: row.command_id,
    requestId: row.request_id,
    digest: Buffer.from(row.digest),
    status: row.status,
    resultPayload: row.result_payload ? Buffer.from(row.result_payload) : null,
    errorCode: row.error_code ?? null,
    createdAt: row.created_at,
    updatedAt: row.updated_at,
    expiresAt: row.expires_at,
  }
}

export class StateStore {
  #db
  #now

  /**
   * @param {object} options
   * @param {string} options.path SQLite file path, or `:memory:`.
   * @param {() => number} [options.now] clock injection.
   */
  constructor({ path, now = Date.now }) {
    this.path = path
    this.#now = now
    if (path !== ':memory:') mkdirSync(dirname(path), { recursive: true })
    try {
      this.#db = new DatabaseSync(path)
    } catch {
      throw new PluginError('internal', 'state store unavailable')
    }
    try {
      this.#db.exec('PRAGMA journal_mode = WAL')
      // Admission must reach durable storage before native side effects begin.
      this.#db.exec('PRAGMA synchronous = FULL')
      this.#db.exec('PRAGMA busy_timeout = 5000')
      this.#migrate()
    } catch {
      try {
        this.#db.close()
      } catch {
        // ignore close failure while reporting the original problem
      }
      throw new PluginError('internal', 'state store migration failed')
    }
  }

  #migrate() {
    const hasMeta=this.#db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name='meta'").get()
    if (hasMeta) {
      const current=this.getMeta('schema_version')
      if (current!==null && current!=='1' && current!=='2' && current!=='3' && current!==String(SCHEMA_VERSION)) throw new PluginError('internal','unsupported state schema')
    }
    this.#db.exec(`
      CREATE TABLE IF NOT EXISTS meta (
        key TEXT PRIMARY KEY,
        value TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS host_identity (
        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
        public_key BLOB NOT NULL,
        fingerprint TEXT NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS peer_keys (
        device_id TEXT PRIMARY KEY,
        public_key BLOB NOT NULL,
        fingerprint TEXT NOT NULL,
        claim_id TEXT,
        paired_at INTEGER NOT NULL,
        display_name TEXT NOT NULL DEFAULT '',
        os TEXT NOT NULL DEFAULT ''
      );
      CREATE TABLE IF NOT EXISTS applied_revocations (
        device_id TEXT PRIMARY KEY,
        replay_id TEXT,
        revoked_at INTEGER NOT NULL,
        applied_at INTEGER NOT NULL
      );
      CREATE TABLE IF NOT EXISTS commands (
        device_id TEXT NOT NULL,
        command_id TEXT NOT NULL,
        request_id TEXT NOT NULL,
        digest BLOB NOT NULL,
        status TEXT NOT NULL,
        result_payload BLOB,
        error_code TEXT,
        created_at INTEGER NOT NULL,
        updated_at INTEGER NOT NULL,
        expires_at INTEGER NOT NULL,
        PRIMARY KEY (device_id, command_id)
      );
      CREATE INDEX IF NOT EXISTS commands_status ON commands (status);
      CREATE TABLE IF NOT EXISTS pending_peers (
        claim_id TEXT PRIMARY KEY, device_id TEXT NOT NULL, public_key BLOB NOT NULL,
        display_name TEXT NOT NULL, os TEXT NOT NULL, expires_at INTEGER NOT NULL
      );
    `)
    const peerColumns = this.#db.prepare('PRAGMA table_info(peer_keys)').all()
    if (!peerColumns.some((column) => column.name === 'display_name')) {
      this.#db.exec(`ALTER TABLE peer_keys ADD COLUMN display_name TEXT NOT NULL DEFAULT ''`)
    }
    if (!peerColumns.some((column) => column.name === 'os')) {
      this.#db.exec(`ALTER TABLE peer_keys ADD COLUMN os TEXT NOT NULL DEFAULT ''`)
    }
    for (const table of ['peer_keys','applied_revocations']) {
      if (!this.#db.prepare(`PRAGMA table_info(${table})`).all().some(column=>column.name==='grant_generation')) {
        this.#db.exec(`ALTER TABLE ${table} ADD COLUMN grant_generation INTEGER NOT NULL DEFAULT 0`)
      }
    }
    this.#db.exec(`
      CREATE TABLE IF NOT EXISTS event_outbox (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        device_id TEXT NOT NULL,
        channel_id TEXT NOT NULL,
        payload BLOB NOT NULL,
        created_at INTEGER NOT NULL
      );
      CREATE INDEX IF NOT EXISTS event_outbox_device ON event_outbox (device_id, id);
    `)
    this.#db.exec('DROP TABLE IF EXISTS session_seqs; DROP TABLE IF EXISTS session_seq_points;')
    const current = this.getMeta('schema_version')
    if (current === null || current === '1' || current === '2' || current === '3') this.setMeta('schema_version', String(SCHEMA_VERSION))
    else if (Number(current) !== SCHEMA_VERSION) {
      throw new PluginError('internal', 'state schema version unsupported')
    }
  }

  #tx(fn) {
    this.#db.exec('BEGIN IMMEDIATE')
    try {
      const result = fn()
      this.#db.exec('COMMIT')
      return result
    } catch (error) {
      try {
        this.#db.exec('ROLLBACK')
      } catch {
        // the original error is more useful
      }
      throw error
    }
  }

  close() {
    try {
      this.#db.close()
    } catch {
      // closing twice is not an error
    }
  }

  // --- meta ---------------------------------------------------------------

  getMeta(key) {
    const row = this.#db.prepare('SELECT value FROM meta WHERE key = ?').get(key)
    return row ? row.value : null
  }

  setMeta(key, value) {
    this.#db.prepare('INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value')
      .run(key, String(value))
  }

  // --- host identity (public reference only) ------------------------------

  saveHostPublicKey(publicKey, fingerprint, now = this.#now()) {
    this.#db.prepare(`
      INSERT INTO host_identity (singleton, public_key, fingerprint, created_at)
      VALUES (1, ?, ?, ?)
      ON CONFLICT(singleton) DO UPDATE SET public_key = excluded.public_key, fingerprint = excluded.fingerprint
    `).run(blob(publicKey), fingerprint, now)
  }

  getHostPublicKey() {
    const row = this.#db.prepare('SELECT public_key, fingerprint, created_at FROM host_identity WHERE singleton = 1').get()
    if (!row) return null
    return {
      publicKey: Buffer.from(row.public_key),
      fingerprint: row.fingerprint,
      createdAt: row.created_at,
    }
  }

  stagePeer({ claimId, deviceId, publicKey, displayName = '', os = '', expiresAt }) {
    this.#db.prepare('DELETE FROM pending_peers WHERE expires_at <= ?').run(this.#now())
    const existing = this.getPendingPeer(claimId)
    if (existing && (existing.deviceId !== deviceId || !Buffer.from(existing.publicKey).equals(publicKey))) throw new PluginError('conflict', 'pending pairing identity changed')
    const count = this.#db.prepare('SELECT count(*) AS count FROM pending_peers').get().count
    if (count >= 64 && !this.getPendingPeer(claimId)) throw new PluginError('rate_limited', 'pending pairing budget exhausted')
    this.#db.prepare(`INSERT INTO pending_peers VALUES (?,?,?,?,?,?) ON CONFLICT(claim_id) DO UPDATE SET expires_at=excluded.expires_at`).run(claimId,deviceId,blob(publicKey),displayName,os,expiresAt)
  }

  getPendingPeer(claimId) {
    const row = this.#db.prepare('SELECT * FROM pending_peers WHERE claim_id=? AND expires_at>?').get(claimId,this.#now())
    return row ? {deviceId:row.device_id,publicKey:Buffer.from(row.public_key),displayName:row.display_name,os:row.os,expiresAt:row.expires_at} : null
  }

  deletePendingPeer(claimId) { this.#db.prepare('DELETE FROM pending_peers WHERE claim_id=?').run(claimId) }

  // --- peer keys ----------------------------------------------------------

  upsertPeerKey({ deviceId, publicKey, fingerprint, claimId = null, grantGeneration = 0, pairedAt = this.#now(), displayName = '', os = '' }) {
    this.#tx(() => {
      this.#db.prepare(`
        INSERT INTO peer_keys (device_id, public_key, fingerprint, claim_id, paired_at, display_name, os, grant_generation)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(device_id) DO UPDATE SET
          public_key = excluded.public_key,
          fingerprint = excluded.fingerprint,
          claim_id = excluded.claim_id,
          paired_at = excluded.paired_at,
          display_name = excluded.display_name,
          os = excluded.os,
          grant_generation = excluded.grant_generation
      `).run(deviceId, blob(publicKey), fingerprint, claimId, pairedAt, displayName, os, grantGeneration)
      // Revocation watermarks survive repair, so delayed COMMIT cannot resurrect an old grant.
    })
  }

  getPeerKey(deviceId) {
    const row = this.#db.prepare('SELECT device_id, public_key, fingerprint, claim_id, paired_at, display_name, os, grant_generation FROM peer_keys WHERE device_id = ?').get(deviceId)
    if (!row) return null
    return {
      deviceId: row.device_id,
      publicKey: Buffer.from(row.public_key),
      fingerprint: row.fingerprint,
      claimId: row.claim_id ?? null,
      grantGeneration: row.grant_generation,
      pairedAt: row.paired_at,
      displayName: row.display_name ?? '',
      os: row.os ?? '',
    }
  }

  listPeerKeys() {
    return this.#db.prepare(`
      SELECT device_id, fingerprint, paired_at, display_name, os FROM peer_keys ORDER BY paired_at DESC, device_id ASC
    `).all().map((row) => ({
      deviceId: row.device_id,
      fingerprint: row.fingerprint,
      pairedAt: row.paired_at,
      displayName: row.display_name ?? '',
      os: row.os ?? '',
    }))
  }

  deletePeerKey(deviceId) {
    this.#db.prepare('DELETE FROM peer_keys WHERE device_id = ?').run(deviceId)
  }

  countPeerKeys() {
    return this.#db.prepare('SELECT COUNT(*) AS n FROM peer_keys').get().n
  }

  // --- revocations --------------------------------------------------------

  /** Delete peer keys and record the applied revocations in one transaction. */
  applyRevocations(deviceIds, { replayId = null, revokedAt = this.#now(), now = this.#now(), generations = {} } = {}) {
    const unique = [...new Set(deviceIds.filter(id=>typeof id==='string'&&id!==''))]
    const applied=[]
    this.#tx(()=>{
      for(const deviceId of unique) {
        const generation=Object.hasOwn(generations,deviceId)?generations[deviceId]:0
        if(!Number.isSafeInteger(generation)||generation<0)throw new PluginError('invalid_argument','invalid grant generation')
        this.#db.prepare(`INSERT INTO applied_revocations(device_id,replay_id,revoked_at,applied_at,grant_generation)
          VALUES(?,?,?,?,?) ON CONFLICT(device_id) DO UPDATE SET replay_id=excluded.replay_id,
          revoked_at=excluded.revoked_at,applied_at=excluded.applied_at,grant_generation=excluded.grant_generation
          WHERE excluded.grant_generation>=applied_revocations.grant_generation`).run(deviceId,replayId,revokedAt,now,generation)
        const peer=this.getPeerKey(deviceId)
        if(peer&&peer.grantGeneration>generation)continue
        this.#db.prepare('DELETE FROM peer_keys WHERE device_id=?').run(deviceId)
        applied.push(deviceId)
      }
    })
    return applied
  }

  isGrantRevoked(deviceId,generation) {
    return this.#db.prepare('SELECT 1 FROM applied_revocations WHERE device_id=? AND grant_generation>=?').get(deviceId,generation)!==undefined
  }

  isRevoked(deviceId) {
    const peer=this.getPeerKey(deviceId)
    return this.isGrantRevoked(deviceId,peer?.grantGeneration??0)
  }

  getRevocation(deviceId) {
    const row = this.#db.prepare('SELECT device_id, replay_id, revoked_at, applied_at, grant_generation FROM applied_revocations WHERE device_id = ?').get(deviceId)
    if (!row) return null
    return { deviceId: row.device_id, grantGeneration: row.grant_generation, replayId: row.replay_id ?? null, revokedAt: row.revoked_at, appliedAt: row.applied_at }
  }

  countRevocations() {
    return this.#db.prepare('SELECT COUNT(*) AS n FROM applied_revocations').get().n
  }

  // --- commands -----------------------------------------------------------

  /**
   * Atomically insert an accepted command or return the existing row. The
   * caller compares digests: a different digest is never allowed to execute.
   */
  createOrGetCommand({ deviceId, commandId, requestId, digest, expiresAt, now = this.#now() }) {
    return this.#tx(() => {
      const existing = this.#db.prepare('SELECT * FROM commands WHERE device_id = ? AND command_id = ?').get(deviceId, commandId)
      if (existing) return { command: rowCommand(existing), created: false }
      this.#db.prepare(`
        INSERT INTO commands (device_id, command_id, request_id, digest, status, result_payload, error_code, created_at, updated_at, expires_at)
        VALUES (?, ?, ?, ?, 'accepted', NULL, NULL, ?, ?, ?)
      `).run(deviceId, commandId, requestId, blob(digest), now, now, expiresAt)
      const row = this.#db.prepare('SELECT * FROM commands WHERE device_id = ? AND command_id = ?').get(deviceId, commandId)
      return { command: rowCommand(row), created: true }
    })
  }

  getCommand(deviceId, commandId) {
    return rowCommand(this.#db.prepare('SELECT * FROM commands WHERE device_id = ? AND command_id = ?').get(deviceId, commandId))
  }

  setCommandStatus({ deviceId, commandId, status, resultPayload = null, errorCode = null, now = this.#now() }) {
    const info = this.#db.prepare(`
      UPDATE commands SET status = ?, result_payload = ?, error_code = ?, updated_at = ?
      WHERE device_id = ? AND command_id = ?
    `).run(status, blob(resultPayload), errorCode, now, deviceId, commandId)
    return Number(info.changes) === 1
  }

  /** Crash recovery: accepted-but-unfinished commands become unknown, never success. */
  recoverAcceptedCommands(now = this.#now()) {
    const info = this.#db.prepare(`
      UPDATE commands SET status = 'unknown', error_code = 'unavailable', updated_at = ?
      WHERE status = 'accepted'
    `).run(now)
    return Number(info.changes)
  }

  countCommands(status) {
    return this.#db.prepare('SELECT COUNT(*) AS n FROM commands WHERE status = ?').get(status).n
  }

  // --- bounded ciphertext outbox -----------------------------------------

  /**
   * Persist one encrypted event envelope before it is published. When either
   * bound is reached the row is not added and the caller stops publishing;
   * clients recover through authoritative history reads.
   */
  addOutbox({ deviceId, channelId, payload, now = this.#now(), perDeviceLimit = Infinity, totalLimit = Infinity }) {
    return this.#tx(() => {
      const total = this.#db.prepare('SELECT COUNT(*) AS n FROM event_outbox').get().n
      if (total >= totalLimit) return { id: null, overflow: true, reason: 'total' }
      const device = this.#db.prepare('SELECT COUNT(*) AS n FROM event_outbox WHERE device_id = ?').get(deviceId).n
      if (device >= perDeviceLimit) return { id: null, overflow: true, reason: 'device' }
      const info = this.#db.prepare(`
        INSERT INTO event_outbox (device_id, channel_id, payload, created_at) VALUES (?, ?, ?, ?)
      `).run(deviceId, channelId, blob(payload), now)
      return { id: Number(info.lastInsertRowid), overflow: false, reason: null }
    })
  }

  listOutbox(limit = 1000) {
    return this.#db.prepare('SELECT id, device_id, channel_id, payload, created_at FROM event_outbox ORDER BY id LIMIT ?').all(limit)
      .map((row) => ({
        id: row.id,
        deviceId: row.device_id,
        channelId: row.channel_id,
        payload: Buffer.from(row.payload),
        createdAt: row.created_at,
      }))
  }

  removeOutbox(id) {
    this.#db.prepare('DELETE FROM event_outbox WHERE id = ?').run(id)
  }

  deleteOutboxForChannel(deviceId, channelId) {
    const info = this.#db.prepare('DELETE FROM event_outbox WHERE device_id = ? AND channel_id = ?').run(deviceId, channelId)
    return Number(info.changes)
  }

  deleteOutboxForDevice(deviceId) {
    this.#db.prepare('DELETE FROM event_outbox WHERE device_id = ?').run(deviceId)
  }

  countOutbox(deviceId = null) {
    if (deviceId === null) return this.#db.prepare('SELECT COUNT(*) AS n FROM event_outbox').get().n
    return this.#db.prepare('SELECT COUNT(*) AS n FROM event_outbox WHERE device_id = ?').get(deviceId).n
  }
}
