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
        CREATE VIRTUAL TABLE IF NOT EXISTS messages_fts USING fts5(
          subject, body_text, sender_addr, tokenize='porter'
        );
        CREATE INDEX IF NOT EXISTS idx_messages_time ON messages(time);
        CREATE INDEX IF NOT EXISTS idx_attachments_message ON attachments(message_id);
        ",
    )?;
    Ok(())
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
