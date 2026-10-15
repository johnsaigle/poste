mod attach;
mod config;
mod db;
mod ingest;
mod metadata;
mod search;
mod text;

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "poste", about = "Local search and management over Proton Mail exports")]
struct Cli {
    /// Root dir containing canonical/ and index.db
    #[arg(long, global = true)]
    root: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Ingest a Proton export directory (mail_<ts>/) into the index
    Ingest {
        /// Export directory; omit and pass --latest to pick the newest
        export_dir: Option<PathBuf>,
        /// Ingest the newest mail_<ts> dir under the archive root
        #[arg(long, conflicts_with = "export_dir")]
        latest: bool,
    },
    /// Keyword (FTS5) search
    Search {
        query: String,
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        after: Option<String>,
        #[arg(long)]
        before: Option<String>,
        #[arg(long)]
        label: Option<String>,
        #[arg(long)]
        has_attachment: bool,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Attachment inventory and extraction
    Attach {
        #[command(subcommand)]
        cmd: AttachCmd,
    },
    /// Show a single message by Proton internal ID
    Show {
        id: String,
        #[arg(long)]
        body: bool,
        #[arg(long)]
        json: bool,
    },
    /// Index statistics
    Stats {
        #[arg(long)]
        json: bool,
    },
    /// Integrity checks
    Doctor {
        #[arg(long)]
        json: bool,
    },
    /// Show resolved configuration (archive root, config file location)
    Config,
}

#[derive(Subcommand)]
enum AttachCmd {
    /// List attachments (bounded, most recent first)
    List {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        ext: Option<String>,
        #[arg(long, default_value_t = 10)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// Extract attachments to a directory (use --dry-run to preview)
    Get {
        #[arg(long)]
        from: Option<String>,
        #[arg(long)]
        ext: Option<String>,
        #[arg(long)]
        out: PathBuf,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let root = config::resolve_root(cli.root);
    db::load_vec_extension();

    match cli.cmd {
        Cmd::Ingest { export_dir, latest } => {
            let export_dir = match (export_dir, latest) {
                (Some(d), false) => d,
                (None, true) => ingest::find_latest_export(&root)?,
                (None, false) => {
                    anyhow::bail!("pass an export directory or --latest");
                }
                (Some(_), true) => unreachable!("clap enforces conflicts_with"),
            };
            let conn = db::open(&root)?;
            let s = ingest::ingest(&conn, &root, &export_dir)
                .context("ingest failed")?;
            println!(
                "scanned {} metadata files: {} inserted, {} already present, {} missing .eml, {} attachments, {} labels",
                s.metadata_files, s.inserted, s.skipped_existing, s.missing_eml, s.attachments, s.labels
            );
        }
        Cmd::Search { query, from, to, after, before, label, has_attachment, limit, json } => {
            let conn = db::open(&root)?;
            let res = search::search(
                &conn,
                &query,
                &search::Filters { from, to, after, before, label, has_attachment, limit },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&res)?);
            } else {
                for h in &res.hits {
                    let date = chrono::DateTime::from_timestamp(h.time, 0)
                        .map(|d| d.format("%Y-%m-%d").to_string())
                        .unwrap_or_default();
                    println!("{}\t{}\t{}\t{}", date, h.sender_addr, h.id, h.subject);
                }
                println!("-- {} of {} hit(s)", res.hits.len(), res.total);
            }
        }
        Cmd::Attach { cmd } => run_attach(&root, cmd)?,
        Cmd::Show { id, body, json } => {
            let conn = db::open(&root)?;
            if let Some(m) = search::show(&conn, &id)? {
                if json {
                    println!("{}", serde_json::to_string_pretty(&m)?);
                } else {
                    println!("id: {}\nfrom: {} <{}>\nsubject: {}", m.id, m.sender_name, m.sender_addr, m.subject);
                    if body {
                        println!("\n{}", m.body_text);
                    }
                }
            } else {
                eprintln!("no message with id {id}");
                std::process::exit(1);
            }
        }
        Cmd::Stats { json } => {
            let conn = db::open(&root)?;
            let s = ingest::stats(&conn)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                println!("messages: {}", s.messages);
                println!("attachments: {}", s.attachments);
                println!("labels: {}", s.labels);
                println!(
                    "range: {:?} -> {:?}",
                    s.oldest.map(ts_to_date),
                    s.newest.map(ts_to_date)
                );
            }
        }
        Cmd::Doctor { json } => {
            let conn = db::open(&root)?;
            let d = ingest::doctor(&conn, &root)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&d)?);
            } else {
                println!("integrity: {}", d.integrity);
                println!("messages: {}", d.messages);
                println!("missing .eml files: {}", d.missing_eml_files);
                println!("fts orphans: {}", d.fts_orphans);
            }
        }
        Cmd::Config => {
            println!(
                "config file: {}",
                config::config_path().map_or_else(
                    || "(none — no config dir on this platform)".to_string(),
                    |p| p.to_string_lossy().into_owned(),
                )
            );
            println!("archive root: {}", root.display());
        }
    }
    Ok(())
}

fn ts_to_date(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

fn run_attach(root: &Path, cmd: AttachCmd) -> Result<()> {
    match cmd {
        AttachCmd::List { from, ext, limit, json } => {
            let conn = db::open(root)?;
            let rows = attach::list(&conn, &attach::AttachFilters { from, ext }, limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&rows)?);
            } else {
                for r in &rows {
                    println!("{}\t{}\t{}\t{}", r.message_id, r.size, r.mime_type, r.filename);
                }
                println!("-- {} attachment(s)", rows.len());
            }
        }
        AttachCmd::Get { from, ext, out, dry_run, json } => {
            let conn = db::open(root)?;
            let files = attach::get(
                &conn,
                root,
                &out,
                &attach::AttachFilters { from, ext },
                dry_run,
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&files)?);
            } else {
                for f in &files {
                    let verb = if dry_run { "would write" } else { "wrote" };
                    println!("{verb} {} -> {}", f.message_id, f.path);
                }
                if dry_run {
                    println!("-- dry run: {} file(s) planned", files.len());
                } else {
                    println!("-- {} file(s) extracted", files.len());
                }
            }
        }
    }
    Ok(())
}
