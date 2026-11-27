CREATE TABLE IF NOT EXISTS pending_routes (
    host_id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    generation INTEGER NOT NULL,
    payload BLOB NOT NULL,
    updated_at INTEGER NOT NULL DEFAULT (unixepoch()*1000)
);
