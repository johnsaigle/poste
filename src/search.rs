use anyhow::Result;
use chrono::NaiveDate;
use rusqlite::Connection;

#[derive(Debug, serde::Serialize)]
pub struct Hit {
    pub id: String,
    /// Short stable alias (`m<rowid>`) — prefer this for chaining.
    pub short_id: String,
    pub subject: String,
    pub sender_addr: String,
    pub time: i64,
    pub score: f64,
    /// Matching excerpt from the body (`""` for filter-only searches).
    pub snippet: String,
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
    // An empty/whitespace-only query is allowed only as "no keyword
    // constraint" so filter-only searches such as `search "" --has-attachment`
    // work instead of failing with `fts5: syntax error near ""`.
    let sanitized = sanitize_query(query);
    let has_query = !sanitized.is_empty();

    let mut conditions: Vec<String> = Vec::new();
    let mut params: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

    if has_query {
        params.push(Box::new(sanitized));
        conditions.push(format!("messages_fts MATCH ?{}", params.len()));
    }
    if let Some(from) = &f.from {
        params.push(Box::new(format!("%{from}%")));
        conditions.push(format!("m.sender_addr LIKE ?{}", params.len()));
    }
    if let Some(to) = &f.to {
        params.push(Box::new(format!("%{to}%")));
        conditions.push(format!(
            "EXISTS (SELECT 1 FROM json_each(m.to_addrs) je
              WHERE json_extract(je.value, '$.Address') LIKE ?{})",
            params.len()
        ));
    }
    if let Some(after) = &f.after {
        params.push(Box::new(parse_date(after, false)?));
        conditions.push(format!("m.time >= ?{}", params.len()));
    }
    if let Some(before) = &f.before {
        params.push(Box::new(parse_date(before, true)?));
        conditions.push(format!("m.time <= ?{}", params.len()));
    }
    if let Some(label) = &f.label {
        // Match by label name, falling back to the raw ID. The archive
        // contains stale label IDs (deleted folders) not present in
        // labels.json; raw-ID matching keeps those queryable.
        params.push(Box::new(label.clone()));
        let n = params.len();
        conditions.push(format!(
            "EXISTS (SELECT 1 FROM json_each(m.label_ids) je
              WHERE je.value = ?{n}
                 OR je.value = (SELECT id FROM labels WHERE name = ?{n}))"
        ));
    }
    if f.has_attachment {
        conditions
            .push("EXISTS (SELECT 1 FROM attachments a WHERE a.message_id = m.id)".to_string());
    }

    // With neither a keyword nor a filter there is nothing to search for.
    // Fail loudly instead of dumping every message (match-all) or silently
    // returning an empty set.
    if conditions.is_empty() {
        anyhow::bail!(
            "empty query with no filters: provide a search term or at least one filter \
             (e.g. --has-attachment)"
        );
    }

    let where_clause = format!("WHERE {}", conditions.join(" AND "));

    let total: i64 = {
        let sql = format!(
            "SELECT COUNT(*) FROM messages_fts fts JOIN messages m ON m.rowid = fts.rowid {where_clause}"
        );
        let mut stmt = conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(std::convert::AsRef::as_ref).collect();
        stmt.query_row(refs.as_slice(), |r| r.get(0))?
    };

    // bm25() is only valid alongside a MATCH query; without one, rank by
    // recency and report a neutral score so ranking stays deterministic.
    // snippet() likewise needs a MATCH; -1 picks the FTS column with the
    // best match, so subjects and sender matches also produce a snippet.
    let (score_expr, snippet_expr, order_by) = if has_query {
        (
            "bm25(messages_fts, 10.0, 1.0, 3.0)",
            "snippet(messages_fts, -1, '<<', '>>', ' ... ', 24)",
            "score",
        )
    } else {
        ("0.0", "''", "m.time DESC")
    };
    let sql = format!(
        "SELECT m.id, m.short_id, m.subject, m.sender_addr, m.time, {score_expr} AS score, \
         {snippet_expr} AS snippet
         FROM messages_fts fts JOIN messages m ON m.rowid = fts.rowid
         {where_clause} ORDER BY {order_by} LIMIT {}",
        f.limit
    );
    let mut stmt = conn.prepare(&sql)?;
    let refs: Vec<&dyn rusqlite::types::ToSql> = params.iter().map(std::convert::AsRef::as_ref).collect();
    let rows = stmt.query_map(refs.as_slice(), |r| {
        Ok(Hit {
            id: r.get(0)?,
            short_id: r.get(1)?,
            subject: r.get(2)?,
            sender_addr: r.get(3)?,
            time: r.get(4)?,
            score: r.get(5)?,
            snippet: r.get(6)?,
        })
    })?;
    let mut hits = Vec::new();
    for row in rows {
        let mut hit = row?;
        hit.snippet = clean_snippet(&hit.snippet);
        hits.push(hit);
    }
    Ok(SearchResult { total, hits })
}

/// Tidy an FTS5 snippet for consumption: html2text renders rules/tables as
/// long runs of box-drawing or space characters, which would otherwise bloat
/// every hit. Collapse any repeated character run to 3 and squash whitespace.
fn clean_snippet(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev = '\0';
    let mut run = 0u32;
    for c in s.chars() {
        if c == prev {
            run += 1;
        } else {
            prev = c;
            run = 0;
        }
        if run < 3 {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, serde::Serialize)]
pub struct FullMessage {
    pub id: String,
    pub short_id: String,
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

/// Look up one message by short alias (`m<rowid>`) or full Proton internal ID.
pub fn show(conn: &Connection, id: &str) -> Result<Option<FullMessage>> {
    let mut stmt = conn.prepare(
        "SELECT id, short_id, external_id, subject, sender_name, sender_addr, to_addrs, cc,
                time, mime_type, label_ids, eml_path, body_text
         FROM messages WHERE id = ?1 OR short_id = ?1",
    )?;
    let mut rows = stmt.query_map([id], |r| {
        Ok(FullMessage {
            id: r.get(0)?,
            short_id: r.get(1)?,
            external_id: r.get(2)?,
            subject: r.get(3)?,
            sender_name: r.get(4)?,
            sender_addr: r.get(5)?,
            to_addrs: r.get(6)?,
            cc: r.get(7)?,
            time: r.get(8)?,
            mime_type: r.get(9)?,
            label_ids: r.get(10)?,
            eml_path: r.get(11)?,
            body_text: r.get(12)?,
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

    #[test]
    fn snippets_collapse_decoration_and_whitespace() {
        let raw = format!("─────  {} Hi  JOHN\n\nthere <<credit>> card", "─".repeat(200));
        let got = clean_snippet(&raw);
        assert_eq!(got.matches('─').count(), 6, "runs must collapse to 3+3: {got}");
        assert!(!got.contains("────"), "long run survived: {got}");
        assert!(!got.contains("  "), "double space survived: {got}");
        assert!(got.contains("<<credit>>"));
    }
}
