use anyhow::{Context, Result};
use rusqlite::Connection;

pub fn open(root: &std::path::Path) -> Result<Connection> {
    std::fs::create_dir_all(root)
        .with_context(|| format!("creating root dir {}", root.display()))?;
    let db_path = root.join("index.db");
    let conn = Connection::open(&db_path)
        .with_context(|| format!("opening {}", db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    init_schema(&conn)?;
    Ok(conn)
}

fn init_schema(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        r"
        CREATE TABLE IF NOT EXISTS messages (
          id TEXT PRIMARY KEY,
          short_id TEXT,
          external_id TEXT,
          subject TEXT,
          sender_name TEXT,
          sender_addr TEXT,
          to_addrs TEXT,
          cc TEXT,
          time INTEGER,
          size INTEGER,
          num_attachments INTEGER,
          mime_type TEXT,
          label_ids TEXT,
          eml_path TEXT,
          body_text TEXT
        );
        CREATE TABLE IF NOT EXISTS attachments (
          id INTEGER PRIMARY KEY,
          message_id TEXT REFERENCES messages(id),
          part_idx INTEGER,
          filename TEXT,
          mime_type TEXT,
          size INTEGER
        );
        CREATE TABLE IF NOT EXISTS labels (
          id TEXT PRIMARY KEY,
          name TEXT
        );
        CREATE TABLE IF NOT EXISTS meta (
          key TEXT PRIMARY KEY,
          value TEXT
        );
        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
          subject, body_text, sender_addr, tokenize='porter'
        );
        CREATE INDEX IF NOT EXISTS idx_messages_time ON messages(time);
        CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_id);
        ",
    )?;

    // Migration for indexes created before short_id existed:
    // CREATE TABLE IF NOT EXISTS cannot add columns to an existing table.
    let has_short_id: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pragma_table_info('messages') WHERE name = 'short_id'",
        [],
        |r| r.get(0),
    )?;
    if has_short_id == 0 {
        conn.execute_batch(
            "ALTER TABLE messages ADD COLUMN short_id TEXT;
             CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_short ON messages(short_id);",
        )?;
    } else {
        // Fresh or already-migrated index: ensure the alias index exists.
        conn.execute_batch(
            "CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_short ON messages(short_id);",
        )?;
    }
    // Backfill any rows lacking an alias (legacy indexes, or rows written by
    // an older binary). The IS NULL check is index-driven, so this is a
    // no-op read for healthy indexes and writes only when needed. Rowids
    // are stable: messages are never deleted or re-inserted (dedup by
    // Proton ID), so 'm<rowid>' is a durable alias.
    let missing: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages WHERE short_id IS NULL",
        [],
        |r| r.get(0),
    )?;
    if missing > 0 {
        conn.execute(
            "UPDATE messages SET short_id = 'm' || rowid WHERE short_id IS NULL",
            [],
        )?;
    }
    Ok(())
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    let mut stmt = conn.prepare("SELECT value FROM meta WHERE key = ?1")?;
    let mut rows = stmt.query([key])?;
    Ok(rows.next()?.map(|r| r.get(0)).transpose()?)
}

/// Load the sqlite-vec extension (Phase 3). Optional: failure is not fatal,
/// keyword search works without it. Auto-extension registration is global,
/// so this only needs to run once per process.
pub fn load_vec_extension() {
    unsafe {
        // sqlite-vec's documented registration path: the init symbol is
        // transmuted into sqlite3_auto_extension's expected signature.
        #[allow(
            unsafe_op_in_unsafe_fn,
            reason = "sqlite3_auto_extension copies the fn pointer; no deref happens here"
        )]
        let rc = rusqlite::ffi::sqlite3_auto_extension(Some(
            std::mem::transmute::<*const (), unsafe extern "C" fn(
                *mut rusqlite::ffi::sqlite3,
                *mut *mut i8,
                *const rusqlite::ffi::sqlite3_api_routines,
            ) -> i32>(sqlite_vec::sqlite3_vec_init as *const ()),
        ));
        if rc != 0 {
            eprintln!("warning: could not register sqlite-vec (rc={rc}); semantic search unavailable");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loads_vec_extension() {
        // Registration is global and idempotent; after it runs, a fresh
        // connection must expose sqlite-vec's functions. If the extension is
        // never registered, `vec_version()` does not exist and this fails.
        load_vec_extension();
        let conn = Connection::open_in_memory().unwrap();
        let v: String = conn
            .query_row("SELECT vec_version()", [], |r| r.get(0))
            .expect("vec_version() missing — extension was not registered");
        assert!(v.starts_with('v'), "unexpected sqlite-vec version: {v}");
    }
}
