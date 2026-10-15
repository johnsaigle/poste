use std::path::PathBuf;
use std::process::Command;

const fn exe() -> &'static str {
    env!("CARGO_BIN_EXE_poste")
}

fn fixture_export() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/export")
}

fn run(root: &std::path::Path, args: &[&str]) -> String {
    let out = Command::new(exe())
        .arg("--root")
        .arg(root)
        .args(args)
        .env_remove("POSTE_CONFIG")
        .output()
        .expect("failed to run poste");
    assert!(
        out.status.success(),
        "command failed: {:?}\nstderr: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Like `run`, but expects a non-zero exit and returns (stdout, stderr).
fn run_fail(root: &std::path::Path, args: &[&str]) -> (String, String) {
    let out = Command::new(exe())
        .arg("--root")
        .arg(root)
        .args(args)
        .env_remove("POSTE_CONFIG")
        .output()
        .expect("failed to run poste");
    assert!(
        !out.status.success(),
        "expected failure, got success: {:?}\nstdout: {}",
        args,
        String::from_utf8_lossy(&out.stdout)
    );
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
    )
}

/// Write a synthetic single-message export into `dir`: `<id>.eml` +
/// `<id>.metadata.json`. Returns the message id.
fn write_msg(
    dir: &std::path::Path,
    id: &str,
    subject: &str,
    token: &str,
    from: &str,
    time: i64,
    label_ids: &[&str],
) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let eml = format!(
        "From: Sender <{from}>\r\nSubject: {subject}\r\n\
         Content-Type: text/plain\r\n\r\nbody about {token}\r\n"
    );
    std::fs::write(dir.join(format!("{id}.eml")), eml).unwrap();
    let labels = label_ids
        .iter()
        .map(|l| format!("\"{l}\""))
        .collect::<Vec<_>>()
        .join(",");
    let meta = format!(
        r#"{{"Version":1,"Payload":{{"ID":"{id}","Subject":"{subject}","Sender":{{"Name":"S","Address":"{from}"}},"ToList":[],"CCList":[],"LabelIDs":[{labels}],"Time":{time},"Size":10,"NumAttachments":0,"Attachments":[],"MIMEType":"text/plain"}}}}"#
    );
    std::fs::write(dir.join(format!("{id}.metadata.json")), meta).unwrap();
    id.to_string()
}

/// Write a message whose metadata claims an attachment named `att_name`,
/// with a matching MIME part in the .eml when `include_part` is true
/// (false simulates a stale index).
fn write_msg_with_attachment(
    dir: &std::path::Path,
    id: &str,
    att_name: &str,
    part_body: &[u8],
    include_part: bool,
) {
    use std::io::Write as _;
    std::fs::create_dir_all(dir).unwrap();
    let mut eml = Vec::new();
    writeln!(eml, "From: Sender <s@x.example>").unwrap();
    writeln!(eml, "Subject: with attachment").unwrap();
    writeln!(eml, "MIME-Version: 1.0").unwrap();
    writeln!(eml, "Content-Type: multipart/mixed; boundary=\"B\"").unwrap();
    writeln!(eml).unwrap();
    writeln!(eml, "--B").unwrap();
    writeln!(eml, "Content-Type: text/plain").unwrap();
    writeln!(eml).unwrap();
    writeln!(eml, "body").unwrap();
    writeln!(eml, "--B").unwrap();
    if include_part {
        writeln!(eml, "Content-Type: application/octet-stream; name=\"{att_name}\"").unwrap();
        writeln!(eml, "Content-Disposition: attachment; filename=\"{att_name}\"").unwrap();
        writeln!(eml).unwrap();
        eml.extend_from_slice(part_body);
        writeln!(eml).unwrap();
    }
    writeln!(eml, "--B--").unwrap();
    std::fs::write(dir.join(format!("{id}.eml")), &eml).unwrap();
    let meta = format!(
        r#"{{"Version":1,"Payload":{{"ID":"{id}","Subject":"with attachment","Sender":{{"Name":"S","Address":"s@x.example"}},"ToList":[],"CCList":[],"LabelIDs":[],"Time":1735689600,"Size":10,"NumAttachments":1,"Attachments":[{{"ID":"a1","Name":"{att_name}","MIMEType":"application/octet-stream","Size":{}}}]}}}}"#,
        part_body.len()
    );
    if std::env::var("POSTE_DEBUG_DUMP").is_ok() {
        std::fs::write("/tmp/opencode/dump_meta.json", &meta).unwrap();
    }
    std::fs::write(dir.join(format!("{id}.metadata.json")), meta).unwrap();
}

#[test]
fn ingest_search_dedup_doctor() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();

    // First ingest: 3 messages
    let out = run(root, &["ingest", fixture_export().to_str().unwrap()]);
    assert!(out.contains("3 inserted"), "got: {out}");
    assert!(out.contains("1 attachments"), "got: {out}");

    // Canonical store has the .eml files, including the leading-dash ID
    assert!(root.join("canonical/AA/AAAabc123==.eml").exists());
    assert!(root.join("canonical/_X/-XYZ987==.eml").exists());

    // Re-ingest is a no-op
    let out = run(root, &["ingest", fixture_export().to_str().unwrap()]);
    assert!(out.contains("0 inserted"), "got: {out}");
    assert!(out.contains("3 already present"), "got: {out}");

    // Keyword search
    let out = run(root, &["search", "invoice", "--json"]);
    assert!(out.contains("AAAabc123=="), "got: {out}");
    assert!(out.contains("\"total\""), "got: {out}");

    // Filtered search: from + label + to + has-attachment
    let out = run(root, &["search", "filing", "--from", "accountant", "--json"]);
    assert!(out.contains("AAAabc123=="), "got: {out}");
    let out = run(root, &["search", "filing", "--label", "Tax", "--json"]);
    assert!(out.contains("AAAabc123=="), "got: {out}");
    let out = run(root, &["search", "staging", "--to", "me@proton.example", "--json"]);
    assert!(out.contains("-XYZ987=="), "got: {out}");
    let out = run(root, &["search", "staging", "--has-attachment", "--json"]);
    assert!(out.contains("-XYZ987=="), "got: {out}");
    let out = run(root, &["search", "staging", "--has-attachment", "--json"]);
    assert!(!out.contains("AAAabc123=="), "no-attachment mail leaked: {out}");

    // Show by ID (leading dash requires `--` before the ID — documented contract)
    let out = run(root, &["show", "--json", "--", "-XYZ987=="]);
    assert!(out.contains("staging"), "got: {out}");

    // Stats + doctor
    let out = run(root, &["stats", "--json"]);
    assert!(out.contains("\"messages\": 3"), "got: {out}");
    let out = run(root, &["doctor", "--json"]);
    assert!(out.contains("\"missing_eml_files\": 0"), "got: {out}");
    assert!(out.contains("\"fts_orphans\": 0"), "got: {out}");
}

#[test]
fn attach_list_and_get() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    run(root, &["ingest", fixture_export().to_str().unwrap()]);

    // List: only the message with a real attachment row
    let out = run(root, &["attach", "list", "--json"]);
    assert!(out.contains("report.pdf"), "got: {out}");
    assert!(out.contains("-XYZ987=="), "got: {out}");

    // List filtered by ext and sender
    let out = run(root, &["attach", "list", "--ext", "pdf", "--json"]);
    assert!(out.contains("report.pdf"), "got: {out}");
    let out = run(root, &["attach", "list", "--ext", "pdf", "--from", "ci", "--json"]);
    assert!(out.contains("report.pdf"), "got: {out}");
    let out = run(root, &["attach", "list", "--ext", "docx", "--json"]);
    assert_eq!(out.trim(), "[]", "got: {out}");

    // Dry run: plans paths, writes nothing
    let out_dir = tmp.path().join("attachments");
    let out = run(
        root,
        &["attach", "get", "--ext", "pdf", "--out", out_dir.to_str().unwrap(), "--dry-run"],
    );
    assert!(out.contains("report.pdf"), "got: {out}");
    assert!(!out_dir.exists(), "dry run created the output dir");

    // Real extraction
    let out = run(
        root,
        &["attach", "get", "--ext", "pdf", "--out", out_dir.to_str().unwrap()],
    );
    assert!(out.contains("wrote"), "got: {out}");
    let written = out_dir.join("report.pdf");
    assert!(written.exists(), "not extracted: {out}");
    assert_eq!(
        std::fs::read(&written).unwrap(),
        b"%PDF-1.4 fixture attachment bytes\n"
    );

    // Second extraction collides -> -1 suffix
    run(root, &["attach", "get", "--ext", "pdf", "--out", out_dir.to_str().unwrap()]);
    assert!(out_dir.join("report-1.pdf").exists());
}

#[test]
fn ingest_latest_picks_newest_export() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fixture = fixture_export().to_str().unwrap().to_string();

    // Two "accounts", each with a mail_<ts> export; newest wins.
    let older = root.join("a@mail.example/mail_20250101_000000");
    let newer = root.join("b@mail.example/mail_20250601_000000");
    std::fs::create_dir_all(&older).unwrap();
    std::fs::create_dir_all(&newer).unwrap();
    for entry in std::fs::read_dir(&fixture).unwrap().flatten() {
        let dst = newer.join(entry.file_name());
        std::fs::copy(entry.path(), &dst).unwrap();
    }
    // Older export contains only one message so we can tell them apart.
    std::fs::copy(
        std::path::PathBuf::from(&fixture).join("AAAabc123==.eml"),
        older.join("AAAabc123==.eml"),
    )
    .unwrap();
    std::fs::copy(
        std::path::PathBuf::from(&fixture).join("AAAabc123==.metadata.json"),
        older.join("AAAabc123==.metadata.json"),
    )
    .unwrap();

    let out = run(root, &["ingest", "--latest"]);
    assert!(out.contains("3 inserted"), "should pick the newer export: {out}");

    // Re-running --latest is a no-op (dedup).
    let out = run(root, &["ingest", "--latest"]);
    assert!(out.contains("0 inserted"), "got: {out}");
}

#[test]
fn config_file_sets_root() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg_dir = tmp.path().join("cfg");
    let archive = tmp.path().join("archive");
    std::fs::create_dir_all(&cfg_dir).unwrap();
    std::fs::write(
        cfg_dir.join("poste.toml"),
        format!("root = \"{}\"\n", archive.display()),
    )
    .unwrap();

    let out = Command::new(exe())
        .arg("config")
        .env("POSTE_CONFIG", cfg_dir.join("poste.toml"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(archive.to_str().unwrap()),
        "config root not resolved: {stdout}"
    );

    // The configured root is used without --root: index.db lands there.
    let out = Command::new(exe())
        .arg("stats")
        .env("POSTE_CONFIG", cfg_dir.join("poste.toml"))
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    assert!(archive.join("index.db").exists(), "index.db not in configured root");
}

#[test]
fn ingest_survives_malformed_metadata_and_missing_eml() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let export = tmp.path().join("export");
    std::fs::create_dir_all(&export).unwrap();
    for entry in std::fs::read_dir(fixture_export()).unwrap().flatten() {
        std::fs::copy(entry.path(), export.join(entry.file_name())).unwrap();
    }
    // Garbage metadata with a matching .eml: parse must fail softly.
    std::fs::write(export.join("bad.eml"), "Subject: x\r\n\r\nbody").unwrap();
    std::fs::write(export.join("bad.metadata.json"), b"{not json").unwrap();
    // Valid metadata with no .eml: counted as missing.
    std::fs::write(
        export.join("LONELY1==.metadata.json"),
        r#"{"Version":1,"Payload":{"ID":"LONELY1==","Subject":"lonely","Time":1735689600}}"#,
    )
    .unwrap();

    let out = run(root, &["ingest", export.to_str().unwrap()]);
    assert!(out.contains("3 inserted"), "valid messages still ingested: {out}");
    assert!(out.contains("1 missing .eml"), "got: {out}");

    // The malformed one is absent from the index.
    let out = run(root, &["stats", "--json"]);
    assert!(out.contains("\"messages\": 3"), "garbage row leaked in: {out}");
}

#[test]
fn attach_get_sanitizes_traversal_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let export = tmp.path().join("export");
    write_msg_with_attachment(
        &export,
        "TRAV1==",
        "../../evil.sh",
        b"#!/bin/sh\nrm -rf /\n",
        true,
    );
    run(root, &["ingest", export.to_str().unwrap()]);

    let out_dir = tmp.path().join("out");
    let out = run(
        root,
        &["attach", "get", "--out", out_dir.to_str().unwrap()],
    );
    assert!(out.contains("evil.sh"), "got: {out}");

    // Landed inside out_dir under its sanitized basename...
    let written = out_dir.join("evil.sh");
    assert_eq!(std::fs::read(&written).unwrap(), b"#!/bin/sh\nrm -rf /\n\n");
    // ...and nothing escaped the output dir.
    assert!(!tmp.path().join("evil.sh").exists());
    assert!(!tmp.path().parent().unwrap().join("evil.sh").exists());
}

#[test]
fn attach_get_errors_on_stale_index() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let export = tmp.path().join("export");
    // Metadata claims ghost.pdf; the .eml has no such MIME part.
    write_msg_with_attachment(&export, "STALE1==", "ghost.pdf", b"x", false);
    run(root, &["ingest", export.to_str().unwrap()]);

    let out_dir = tmp.path().join("out");
    let (stdout, stderr) = run_fail(
        root,
        &["attach", "get", "--out", out_dir.to_str().unwrap()],
    );
    assert!(stdout.is_empty(), "got: {stdout}");
    assert!(stderr.contains("no MIME part named"), "got: {stderr}");
    assert!(!out_dir.join("ghost.pdf").exists());
}

#[test]
fn search_total_limit_before_and_label_fallback() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let export = tmp.path().join("export");
    let base = 1_735_689_600; // 2025-01-01
    for i in 0..5u32 {
        let day = base + i64::from(i) * 86_400;
        let id = format!("W{i}====");
        write_msg(
            &export,
            &id,
            &format!("widget order {i}"),
            "widget",
            &format!("w{i}@x.example"),
            day,
            if i == 4 { &["kStale"] } else { &[] },
        );
    }
    // labels.json deliberately does NOT contain kStale — the raw-ID
    // fallback must still match it.
    std::fs::write(export.join("labels.json"), br#"{"5":"All Mail"}"#).unwrap();
    run(root, &["ingest", export.to_str().unwrap()]);

    // total reflects all matches; hits are bounded by --limit.
    let out = run(root, &["search", "widget", "--limit", "2", "--json"]);
    assert!(out.contains("\"total\": 5"), "got: {out}");
    assert_eq!(out.matches("\"id\"").count(), 2, "limit not applied: {out}");

    // --before is inclusive of the end-of-day.
    let out = run(root, &["search", "widget", "--before", "2025-01-03", "--json"]);
    assert!(out.contains("\"total\": 3"), "got: {out}");

    // Label by name...
    let out = run(root, &["search", "widget", "--label", "All Mail", "--json"]);
    assert!(out.contains("\"total\": 0"), "unlabeled mail matched: {out}");
    // ...and by raw ID absent from labels.json (stale-folder fallback).
    let out = run(root, &["search", "widget", "--label", "kStale", "--json"]);
    assert!(out.contains("W4===="), "raw-ID label fallback failed: {out}");
}

#[test]
fn show_missing_id_exits_nonzero() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    run(root, &["ingest", fixture_export().to_str().unwrap()]);
    let (stdout, stderr) = run_fail(root, &["show", "--", "NOSUCHID=="]);
    assert!(stdout.is_empty(), "got: {stdout}");
    assert!(stderr.contains("no message with id"), "got: {stderr}");
}

#[test]
fn ingest_latest_without_exports_fails_cleanly() {
    let tmp = tempfile::tempdir().unwrap();
    let (stdout, stderr) = run_fail(tmp.path(), &["ingest", "--latest"]);
    assert!(stdout.is_empty(), "got: {stdout}");
    assert!(stderr.contains("no mail_<ts>"), "got: {stderr}");
}

#[test]
fn doctor_detects_missing_eml_and_fts_orphans() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    run(root, &["ingest", fixture_export().to_str().unwrap()]);

    // Remove a canonical .eml behind the index's back.
    std::fs::remove_file(root.join("canonical/AA/AAAabc123==.eml")).unwrap();
    let out = run(root, &["doctor", "--json"]);
    assert!(out.contains("\"missing_eml_files\": 1"), "got: {out}");

    // Plant an FTS row with no matching message.
    let conn = rusqlite::Connection::open(root.join("index.db")).unwrap();
    conn.execute(
        "INSERT INTO messages_fts (rowid, subject, body_text, sender_addr)
         VALUES (99999, 'orphan', 'orphan', 'o@x.example')",
        [],
    )
    .unwrap();
    drop(conn);
    let out = run(root, &["doctor", "--json"]);
    assert!(out.contains("\"fts_orphans\": 1"), "got: {out}");
}

#[test]
fn ingest_twice_is_stable_in_db_and_canonical_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let fixture = fixture_export();

    run(root, &["ingest", fixture.to_str().unwrap()]);
    run(root, &["ingest", fixture.to_str().unwrap()]);

    let conn = rusqlite::Connection::open(root.join("index.db")).unwrap();
    let count = |sql: &str| -> i64 { conn.query_row(sql, [], |r| r.get(0)).unwrap() };
    assert_eq!(count("SELECT COUNT(*) FROM messages"), 3);
    assert_eq!(count("SELECT COUNT(*) FROM attachments"), 1);
    assert_eq!(count("SELECT COUNT(*) FROM messages_fts"), 3);
    assert_eq!(count("SELECT COUNT(*) FROM labels"), 4);
    drop(conn);

    // Canonical copies are byte-identical to the fixture sources.
    for name in ["AAAabc123==.eml", "-XYZ987==.eml", "CCCdef456==.eml"] {
        let src = std::fs::read(fixture.join(name)).unwrap();
        let first2: String = name
            .chars()
            .take(2)
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let dst = std::fs::read(root.join("canonical").join(&first2).join(name)).unwrap();
        assert_eq!(src, dst, "canonical bytes differ for {name}");
    }
}

#[test]
fn malformed_config_warns_and_falls_back() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = tmp.path().join("broken.toml");
    std::fs::write(&cfg, "root = [not valid toml").unwrap();

    let out = Command::new(exe())
        .arg("config")
        .env("POSTE_CONFIG", &cfg)
        .output()
        .unwrap();
    assert!(out.status.success(), "malformed config must not be fatal");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warning: ignoring"), "got: {stderr}");
    assert!(
        stdout.contains("Documents/email-backup"),
        "did not fall back to default root: {stdout}"
    );
}
