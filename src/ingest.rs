use std::path::Path;

use anyhow::{Context, Result};
use rusqlite::Connection;
use walkdir::WalkDir;

use crate::metadata::{attachment_fields, parse_labels, parse_metadata, sanitize_id_component};
use crate::text::extract_body;

/// Find the newest `mail_<ts>` export dir under the archive root
/// (`<root>/<account>/mail_<ts>/`). Proton timestamps sort lexicographically.
pub fn find_latest_export(root: &Path) -> Result<std::path::PathBuf> {
    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    let entries = std::fs::read_dir(root)
        .with_context(|| format!("reading archive root {}", root.display()))?;
    for account in entries {
        let account = account?.path();
        if !account.is_dir() || account.file_name() == Some(std::ffi::OsStr::new("canonical")) {
            continue;
        }
        let Ok(mails) = std::fs::read_dir(&account) else {
            continue;
        };
        for mail in mails {
            let mail = mail?.path();
            let is_mail_ts = mail.file_name().is_some_and(|n| {
                n.to_string_lossy().starts_with("mail_")
            });
            if mail.is_dir() && is_mail_ts {
                candidates.push(mail);
            }
        }
    }
    candidates.sort();
    candidates.pop().context("no mail_<ts> export dirs found under archive root")
}

#[derive(Debug, Default, serde::Serialize)]
pub struct IngestStats {
    pub metadata_files: usize,
    pub inserted: usize,
    pub skipped_existing: usize,
    pub missing_eml: usize,
    pub attachments: usize,
    pub labels: usize,
}

/// Ingest a Proton export directory into the canonical store + index.
/// Incremental: message IDs already present are skipped.
pub fn ingest(conn: &Connection, root: &Path, export_dir: &Path) -> Result<IngestStats> {
    let mut stats = IngestStats::default();
    let canonical = root.join("canonical");

    if let Ok(labels_bytes) = std::fs::read(export_dir.join("labels.json")) {
        for (id, name) in parse_labels(&labels_bytes) {
            conn.execute(
                "INSERT OR REPLACE INTO labels (id, name) VALUES (?1, ?2)",
                [&id, &name],
            )?;
            stats.labels += 1;
        }
    }

    for entry in WalkDir::new(export_dir).max_depth(3).into_iter().filter_map(std::result::Result::ok) {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let stem = match path.file_stem().and_then(|s| s.to_str()) {
            Some(s) if s.ends_with(".metadata") => s.strip_suffix(".metadata").unwrap().to_string(),
            _ => continue, // labels.json and anything else
        };
        stats.metadata_files += 1;

        let eml_src = path.with_file_name(format!("{stem}.eml"));
        if !eml_src.exists() {
            stats.missing_eml += 1;
            continue;
        }

        let meta_bytes = std::fs::read(path)?;
        let meta = match parse_metadata(&meta_bytes) {
            Ok(m) => m,
            Err(e) => {
                eprintln!("warning: skipping {}: {e}", path.display());
                continue;
            }
        };
        let id = meta.payload.id.clone();

        let dir2 = sanitize_id_component(&id.chars().take(2).collect::<String>());
        let canonical_dir = canonical.join(&dir2);
        std::fs::create_dir_all(&canonical_dir)?;
        let eml_rel = Path::new("canonical").join(&dir2).join(format!("{id}.eml"));
        let eml_dst = root.join(&eml_rel);
        if !eml_dst.exists() {
            std::fs::copy(&eml_src, &eml_dst)?;
        }

        let body = extract_body(&std::fs::read(&eml_src)?);
        let sender = meta.payload.sender.clone().unwrap_or_default();
        let to_json = serde_json::to_string(&meta.payload.to_list)?;
        let cc_json = serde_json::to_string(&meta.payload.cc_list)?;
        let labels_json = serde_json::to_string(&meta.payload.label_ids)?;

        conn.execute(
            "INSERT INTO messages (id, external_id, subject, sender_name, sender_addr,
             to_addrs, cc, time, size, num_attachments, mime_type, label_ids, eml_path, body_text)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)
             ON CONFLICT(id) DO NOTHING",
            rusqlite::params![
                id,
                meta.payload.external_id,
                meta.payload.subject,
                sender.name,
                sender.address,
                to_json,
                cc_json,
                meta.payload.time,
                meta.payload.size,
                meta.payload.num_attachments,
                meta.payload.mime_type,
                labels_json,
                eml_rel.to_string_lossy(),
                body,
            ],
        )?;
        if conn.changes() == 0 {
            stats.skipped_existing += 1;
            continue;
        }
        stats.inserted += 1;

        let msg_rowid = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO messages_fts (rowid, subject, body_text, sender_addr)
             VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                msg_rowid,
                meta.payload.subject,
                body,
                sender.address,
            ],
        )?;

        for (idx, att) in meta.payload.attachments.iter().enumerate() {
            let (filename, mime, size) = attachment_fields(att);
            conn.execute(
                "INSERT INTO attachments (message_id, part_idx, filename, mime_type, size)
                 VALUES (?1,?2,?3,?4,?5)",
                rusqlite::params![id, i64::try_from(idx).unwrap_or_default(), filename, mime, size],
            )?;
            stats.attachments += 1;
        }
    }

    Ok(stats)
}

#[derive(Debug, serde::Serialize)]
pub struct Counters {
    pub messages: i64,
    pub attachments: i64,
    pub labels: i64,
    pub oldest: Option<i64>,
    pub newest: Option<i64>,
}

pub fn stats(conn: &Connection) -> Result<Counters> {
    let q = |sql: &str| -> Result<i64> {
        Ok(conn.query_row(sql, [], |r| r.get(0))?)
    };
    let qopt = |sql: &str| -> Result<Option<i64>> {
        Ok(conn.query_row(sql, [], |r| r.get::<_, Option<i64>>(0))?)
    };
    Ok(Counters {
        messages: q("SELECT COUNT(*) FROM messages")?,
        attachments: q("SELECT COUNT(*) FROM attachments")?,
        labels: q("SELECT COUNT(*) FROM labels")?,
        oldest: qopt("SELECT MIN(time) FROM messages")?,
        newest: qopt("SELECT MAX(time) FROM messages")?,
    })
}

#[derive(Debug, serde::Serialize)]
pub struct DoctorReport {
    pub integrity: String,
    pub messages: i64,
    pub missing_eml_files: i64,
    pub fts_orphans: i64,
}

pub fn doctor(conn: &Connection, root: &Path) -> Result<DoctorReport> {
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    let messages: i64 = conn.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0))?;

    let mut missing = 0i64;
    let mut stmt = conn.prepare("SELECT eml_path FROM messages")?;
    let paths: Vec<String> = stmt
        .query_map([], |r| r.get(0))?
        .filter_map(std::result::Result::ok)
        .collect();
    for p in paths {
        if !root.join(&p).exists() {
            missing += 1;
        }
    }

    let fts_orphans: i64 = conn.query_row(
        "SELECT COUNT(*) FROM messages_fts f LEFT JOIN messages m ON m.rowid = f.rowid
         WHERE m.id IS NULL",
        [],
        |r| r.get(0),
    )?;

    Ok(DoctorReport { integrity, messages, missing_eml_files: missing, fts_orphans })
}
