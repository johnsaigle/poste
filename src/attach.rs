use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use mailparse::{parse_mail, ParsedMail};
use rusqlite::Connection;

use crate::metadata::sanitize_filename;

#[derive(Debug, serde::Serialize)]
pub struct AttachRow {
    pub message_id: String,
    pub part_idx: i64,
    pub filename: String,
    pub mime_type: String,
    pub size: i64,
}

pub struct AttachFilters {
    pub from: Option<String>,
    pub ext: Option<String>,
}

/// Build the shared SELECT for attachment rows (with message join for
/// filtering and `eml_path` resolution). Params are appended in order; the
/// returned SQL references them as ?N positionally.
fn select_sql(f: &AttachFilters, params: &mut Vec<Box<dyn rusqlite::types::ToSql>>) -> String {
    let mut sql = String::from(
        "SELECT a.message_id, a.part_idx, a.filename, a.mime_type, a.size, m.eml_path
         FROM attachments a JOIN messages m ON m.id = a.message_id WHERE 1=1",
    );
    if let Some(from) = &f.from {
        let _ = write!(sql, " AND m.sender_addr LIKE ?{}", params.len() + 1);
        params.push(Box::new(format!("%{from}%")));
    }
    if let Some(ext) = &f.ext {
        let e = ext.trim_start_matches('.').to_ascii_lowercase();
        // Match by filename extension or by mime subtype.
        let _ = write!(
            sql,
            " AND (LOWER(a.filename) LIKE ?{fn_} OR LOWER(a.mime_type) LIKE ?{mn})",
            fn_ = params.len() + 1,
            mn = params.len() + 2,
        );
        params.push(Box::new(format!("%.{e}")));
        params.push(Box::new(format!("%/{e}")));
    }
    sql
}

fn query_rows(conn: &Connection, f: &AttachFilters) -> Result<Vec<(AttachRow, String)>> {
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let sql = select_sql(f, &mut params);
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(AsRef::as_ref).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok((
            AttachRow {
                message_id: r.get(0)?,
                part_idx: r.get(1)?,
                filename: r.get(2)?,
                mime_type: r.get(3)?,
                size: r.get(4)?,
            },
            r.get::<_, String>(5)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

pub fn list(conn: &Connection, f: &AttachFilters, limit: usize) -> Result<Vec<AttachRow>> {
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    let mut sql = select_sql(f, &mut params);
    let _ = write!(sql, " ORDER BY m.time DESC LIMIT {limit}");
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(AsRef::as_ref).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok(AttachRow {
            message_id: r.get(0)?,
            part_idx: r.get(1)?,
            filename: r.get(2)?,
            mime_type: r.get(3)?,
            size: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[derive(Debug, serde::Serialize)]
pub struct Extracted {
    pub message_id: String,
    pub filename: String,
    pub path: String,
    pub written: bool,
}

/// Extract attachments matching the filters into `out_dir`. With `dry_run`,
/// nothing is written and no directories are created; the planned
/// destination paths are returned with `written: false`.
pub fn get(
    conn: &Connection,
    root: &Path,
    out_dir: &Path,
    f: &AttachFilters,
    dry_run: bool,
) -> Result<Vec<Extracted>> {
    let rows = query_rows(conn, f)?;
    let mut out = Vec::new();
    if !dry_run {
        std::fs::create_dir_all(out_dir)
            .with_context(|| format!("creating output dir {}", out_dir.display()))?;
    }
    for (row, eml_rel) in rows {
        let safe = sanitize_filename(&row.filename);
        let dest = unique_path(out_dir, &safe);
        if dry_run {
            out.push(Extracted {
                message_id: row.message_id,
                filename: safe,
                path: dest.to_string_lossy().into_owned(),
                written: false,
            });
            continue;
        }
        let eml_path = root.join(&eml_rel);
        let eml = std::fs::read(&eml_path)
            .with_context(|| format!("reading {}", eml_path.display()))?;
        let mail = parse_mail(&eml)
            .with_context(|| format!("parsing {}", eml_path.display()))?;
        let part = find_part(&mail, &row.filename).ok_or_else(|| {
            anyhow::anyhow!(
                "no MIME part named {:?} in {} (index may be stale)",
                row.filename,
                eml_path.display()
            )
        })?;
        let bytes = part.get_body_raw().context("decoding attachment part")?;
        std::fs::write(&dest, &bytes)
            .with_context(|| format!("writing {}", dest.display()))?;
        out.push(Extracted {
            message_id: row.message_id,
            filename: safe,
            path: dest.to_string_lossy().into_owned(),
            written: true,
        });
    }
    Ok(out)
}

/// Resolve `dir/name`, appending -1, -2, ... before the extension on
/// collision (per the CLI contract).
fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    for i in 1u64.. {
        let cand = dir.join(format!("{stem}-{i}{ext}"));
        if !cand.exists() {
            return cand;
        }
    }
    unreachable!("exhausted collision suffixes")
}

fn part_filename(p: &ParsedMail) -> Option<String> {
    let cd = p.get_content_disposition();
    if let Some((_, v)) = cd
        .params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("filename"))
    {
        return Some(v.clone());
    }
    p.ctype
        .params
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("name"))
        .map(|(_, v)| v.clone())
}

fn find_part<'a>(part: &'a ParsedMail<'a>, filename: &str) -> Option<&'a ParsedMail<'a>> {
    if part.subparts.is_empty() {
        return if part_filename(part).as_deref() == Some(filename) {
            Some(part)
        } else {
            None
        };
    }
    for sub in &part.subparts {
        if let Some(p) = find_part(sub, filename) {
            return Some(p);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collision_suffixes_go_before_extension() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path();
        assert_eq!(unique_path(dir, "a.pdf"), dir.join("a.pdf"));
        std::fs::write(dir.join("a.pdf"), b"x").unwrap();
        assert_eq!(unique_path(dir, "a.pdf"), dir.join("a-1.pdf"));
        std::fs::write(dir.join("a-1.pdf"), b"x").unwrap();
        assert_eq!(unique_path(dir, "a.pdf"), dir.join("a-2.pdf"));
        assert_eq!(unique_path(dir, "noext"), dir.join("noext"));
        std::fs::write(dir.join("noext"), b"x").unwrap();
        assert_eq!(unique_path(dir, "noext"), dir.join("noext-1"));
    }

    #[test]
    fn finds_part_by_filename() {
        let eml = b"From: a@b.c\r\nSubject: t\r\n\
MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"B\"\r\n\r\n\
--B\r\nContent-Type: text/plain\r\n\r\nbody\r\n\
--B\r\nContent-Type: application/pdf; name=\"r.pdf\"\r\n\
Content-Disposition: attachment; filename=\"r.pdf\"\r\n\r\nPDFBYTES\r\n\
--B--\r\n";
        let mail = parse_mail(eml).unwrap();
        let part = find_part(&mail, "r.pdf").unwrap();
        assert_eq!(part.get_body_raw().unwrap(), b"PDFBYTES\r\n");
        assert!(find_part(&mail, "other.pdf").is_none());
    }
}
