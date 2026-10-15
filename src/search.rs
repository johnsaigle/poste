use std::fmt::Write as _;

use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::Connection;

#[derive(Debug, serde::Serialize)]
pub struct Hit {
    pub id: String,
    pub subject: String,
    pub sender_addr: String,
    pub time: i64,
    pub score: f64,
}

pub struct Filters {
    pub from: Option<String>,
    pub to: Option<String>,
    pub after: Option<String>,
    pub before: Option<String>,
    pub label: Option<String>,
    pub has_attachment: bool,
    pub limit: usize,
}

fn parse_date(s: &str, end_of_day: bool) -> Result<i64> {
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|e| anyhow::anyhow!("bad date {s:?} (expected YYYY-MM-DD): {e}"))?;
    let t = d.and_hms_opt(0, 0, 0).unwrap().and_utc();
    Ok(if end_of_day {
        t.timestamp() + 86_399
    } else {
        t.timestamp()
    })
}

/// Quote each whitespace-separated token so user input is treated as phrase
/// terms, not FTS5 syntax.
fn sanitize_query(q: &str) -> String {
    q.split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, serde::Serialize)]
pub struct SearchResult {
    pub total: i64,
    pub hits: Vec<Hit>,
}

pub fn search(conn: &Connection, query: &str, f: &Filters) -> Result<SearchResult> {
    let mut where_clause = String::from("WHERE messages_fts MATCH ?1");
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = vec![Box::new(sanitize_query(query))];

    if let Some(from) = &f.from {
        let _ = write!(where_clause, " AND m.sender_addr LIKE ?{}", params.len() + 1);
        params.push(Box::new(format!("%{from}%")));
    }
    if let Some(to) = &f.to {
        let _ = write!(
            where_clause,
            " AND EXISTS (SELECT 1 FROM json_each(m.to_addrs) je
              WHERE json_extract(je.value, '$.Address') LIKE ?{})",
            params.len() + 1
        );
        params.push(Box::new(format!("%{to}%")));
    }
    if let Some(after) = &f.after {
        let _ = write!(where_clause, " AND m.time >= ?{}", params.len() + 1);
        params.push(Box::new(parse_date(after, false)?));
    }
    if let Some(before) = &f.before {
        let _ = write!(where_clause, " AND m.time <= ?{}", params.len() + 1);
        params.push(Box::new(parse_date(before, true)?));
    }
    if let Some(label) = &f.label {
        // Match by label name, falling back to the raw ID. The archive
        // contains stale label IDs (deleted folders) not present in
        // labels.json; raw-ID matching keeps those queryable.
        let _ = write!(
            where_clause,
            " AND EXISTS (SELECT 1 FROM json_each(m.label_ids) je
              WHERE je.value = ?{n}
                 OR je.value = (SELECT id FROM labels WHERE name = ?{n}))",
            n = params.len() + 1
        );
        params.push(Box::new(label.clone()));
    }
    if f.has_attachment {
        where_clause.push_str(
            " AND EXISTS (SELECT 1 FROM attachments a WHERE a.message_id = m.id)",
        );
    }

    let total: i64 = {
        let sql = format!(
            "SELECT COUNT(*) FROM messages_fts fts JOIN messages m ON m.rowid = fts.rowid {where_clause}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(std::convert::AsRef::as_ref).collect();
        stmt.query_row(refs.as_slice(), |r| r.get(0))?
    };

    let sql = format!(
        "SELECT m.id, m.subject, m.sender_addr, m.time, bm25(messages_fts, 10.0, 1.0, 3.0) AS score
         FROM messages_fts fts JOIN messages m ON m.rowid = fts.rowid
         {where_clause} ORDER BY score LIMIT {}",
        f.limit
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(std::convert::AsRef::as_ref).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok(Hit {
            id: r.get(0)?,
            subject: r.get(1)?,
            sender_addr: r.get(2)?,
            time: r.get(3)?,
            score: r.get(4)?,
        })
    })?;
    let mut hits = Vec::new();
    for row in rows {
        hits.push(row?);
    }
    Ok(SearchResult { total, hits })
}

#[derive(Debug, serde::Serialize)]
pub struct FullMessage {
    pub id: String,
    pub external_id: String,
    pub subject: String,
    pub sender_name: String,
    pub sender_addr: String,
    pub to_addrs: String,
    pub cc: String,
    pub time: i64,
    pub mime_type: String,
    pub label_ids: String,
    pub eml_path: String,
    pub body_text: String,
}

pub fn show(conn: &Connection, id: &str) -> Result<Option<FullMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, external_id, subject, sender_name, sender_addr, to_addrs, cc,
                time, mime_type, label_ids, eml_path, body_text
         FROM messages WHERE id = ?1",
    )?;
    let mut rows = stmt.query_map([id], |r| {
        Ok(FullMessage {
            id: r.get(0)?,
            external_id: r.get(1)?,
            subject: r.get(2)?,
            sender_name: r.get(3)?,
            sender_addr: r.get(4)?,
            to_addrs: r.get(5)?,
            cc: r.get(6)?,
            time: r.get(7)?,
            mime_type: r.get(8)?,
            label_ids: r.get(9)?,
            eml_path: r.get(10)?,
            body_text: r.get(11)?,
        })
    })?;
    Ok(rows.next().transpose()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_tokens() {
        assert_eq!(sanitize_query("invoice 2024"), "\"invoice\" \"2024\"");
        assert_eq!(sanitize_query("a\"b"), "\"a\"\"b\"");
    }
}
