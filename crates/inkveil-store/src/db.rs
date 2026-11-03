//! SQLite connection setup, schema and migrations.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::StoreError;

/// Schema version stored in `PRAGMA user_version`.
const SCHEMA_VERSION: u32 = 3;

const SCHEMA_V1: &str = "
    CREATE TABLE meta (
        key   TEXT PRIMARY KEY,
        value BLOB NOT NULL
    ) STRICT;

    CREATE TABLE notes (
        id          BLOB PRIMARY KEY,
        sealed_meta BLOB NOT NULL,
        sealed_body BLOB NOT NULL
    ) STRICT;

    CREATE TABLE revisions (
        rev         INTEGER PRIMARY KEY AUTOINCREMENT,
        note_id     BLOB NOT NULL,
        sealed_meta BLOB NOT NULL,
        sealed_body BLOB NOT NULL
    ) STRICT;
    CREATE INDEX revisions_by_note ON revisions (note_id, rev);

    CREATE TABLE state (
        key    TEXT PRIMARY KEY,
        sealed BLOB NOT NULL
    ) STRICT;
";

/// Version 2: each save gets a random id that is bound into both of its sealed halves.
const MIGRATE_TO_V2: &str = "
    ALTER TABLE notes ADD COLUMN save_id BLOB;
    ALTER TABLE revisions ADD COLUMN save_id BLOB;
";

/// Version 3: encrypted attachments (images), referenced from notes by id.
const MIGRATE_TO_V3: &str = "
    CREATE TABLE blobs (
        id          BLOB PRIMARY KEY,
        fingerprint BLOB NOT NULL UNIQUE,
        sealed      BLOB NOT NULL,
        created_ms  INTEGER NOT NULL
    ) STRICT;
";

/// Opens a vault database, creating the file only when `create` is true.
pub(crate) fn open(path: &Path, create: bool) -> Result<Connection, StoreError> {
    let mut flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    if create {
        flags |= OpenFlags::SQLITE_OPEN_CREATE;
    }
    let conn = Connection::open_with_flags(path, flags)?;
    configure(&conn)?;
    Ok(conn)
}

/// Applies the settings that make every commit durable and keep plaintext off the disk.
fn configure(conn: &Connection) -> Result<(), StoreError> {
    // WAL keeps the main file intact until a checkpoint, and FULL fsyncs every commit.
    let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(StoreError::Corrupt("database refused WAL mode"));
    }
    conn.execute_batch(
        "PRAGMA synchronous = FULL;
         PRAGMA fullfsync = ON;
         PRAGMA checkpoint_fullfsync = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA secure_delete = ON;
         PRAGMA cell_size_check = ON;
         PRAGMA foreign_keys = ON;
         PRAGMA trusted_schema = OFF;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(())
}

/// Creates the current schema in an empty database.
pub(crate) fn create_schema(conn: &Connection) -> Result<(), StoreError> {
    conn.execute_batch(SCHEMA_V1)?;
    conn.execute_batch(MIGRATE_TO_V2)?;
    conn.execute_batch(MIGRATE_TO_V3)?;
    conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    Ok(())
}

/// Checks the schema version and applies structural migrations. Refuses databases from newer
/// releases. Record re-sealing, which needs the key, happens later at unlock.
pub(crate) fn check_and_migrate(conn: &mut Connection) -> Result<(), StoreError> {
    let version: u32 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    match version {
        SCHEMA_VERSION => Ok(()),
        1 | 2 => {
            let tx = conn.transaction()?;
            if version < 2 {
                tx.execute_batch(MIGRATE_TO_V2)?;
            }
            tx.execute_batch(MIGRATE_TO_V3)?;
            tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            tx.commit()?;
            Ok(())
        }
        0 => Err(StoreError::NotFound),
        newer if newer > SCHEMA_VERSION => Err(StoreError::UnsupportedVersion(newer)),
        _ => Err(StoreError::Corrupt("unknown schema version")),
    }
}

pub(crate) fn read_meta(conn: &Connection, key: &str) -> Result<Option<Vec<u8>>, StoreError> {
    Ok(conn
        .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
        .optional()?)
}

pub(crate) fn write_meta(conn: &Connection, key: &str, value: &[u8]) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        rusqlite::params![key, value],
    )?;
    Ok(())
}

/// Runs SQLite's fast structural check. Returns `true` when the database is healthy.
pub(crate) fn quick_check(conn: &Connection) -> Result<bool, StoreError> {
    let result: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    Ok(result == "ok")
}
