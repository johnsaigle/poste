# Poste — Development Plan

**Date:** 2026-10-07
**Status:** Phase 0 complete; Phase 1 nearly complete (see checkboxes).
**Context:** `email-archive-strategy-2026-10-05.md` (recon), research session 2026-10-07 (OSS landscape, embeddings).
**Language:** Rust. **Working name:** `poste` (placeholder — rename freely).

---

## What this is

A local-first search and management layer over Proton Mail exports. Single Rust
binary + one SQLite file. No server, no daemon. Designed so an agent harness
(opencode, oh-my-pi) is the primary UX via a documented CLI contract, with
humans using the same CLI directly.

Core jobs:

1. Ingest Proton export dirs (`<ID>.eml` + `<ID>.metadata.json` + `labels.json`) into a canonical store + index, deduped by Proton internal ID.
2. Hybrid search: FTS5 (BM25) + sqlite-vec (embeddings) fused with RRF.
3. Attachment inventory and deterministic extraction (`--dry-run` first).
4. Thin verbs for tagging/labeling so agents can organize mail.

Explicitly **not** in scope: sending mail, live sync (no paid Proton plan →
exports only), a web UI, attachment text extraction (paperless-ngx's job).

## Architecture

```
L3  Agent harness — UX = skill file documenting the CLI
L2  poste CLI (this tool)
L1  ~/Documents/email-backup/canonical/<first2>/<ID>.eml
    ~/Documents/email-backup/index.db  (SQLite: metadata + FTS5 + sqlite-vec)
L0  ~/Documents/email-backup/<account>/mail_<ts>/  (immutable raw exports)
```

Single crate to start (`src/main.rs` + modules); split into lib+bin only when
an MCP wrapper or second frontend actually exists.

### Dependencies (proposed)

| Purpose | Crate |
|---|---|
| CLI | `clap` (derive) |
| SQLite | `rusqlite` (bundled — includes FTS5) |
| Vectors | `sqlite-vec` (load as extension at runtime) |
| JSON | `serde`, `serde_json` |
| EML/MIME parsing | `mailparse` |
| HTML → text | `html2text` |
| Walking exports | `walkdir` |
| Embeddings client | `reqwest` (blocking, rustls) against TabbyAPI's OpenAI-compatible `/v1/embeddings` |
| Errors | `anyhow` + `thiserror` |

### Schema (v1)

```sql
CREATE TABLE messages (
  id TEXT PRIMARY KEY,            -- Proton internal ID (the base64 filename stem)
  external_id TEXT,               -- RFC Message-ID
  subject TEXT, sender_name TEXT, sender_addr TEXT,
  to_addrs TEXT, cc TEXT,         -- JSON arrays
  time INTEGER,                   -- unix ts from Payload.Time
  size INTEGER, num_attachments INTEGER, mime_type TEXT,
  label_ids TEXT,                 -- JSON array
  eml_path TEXT,                  -- relative to canonical root
  body_text TEXT                  -- HTML-stripped, qp-decoded
);
CREATE TABLE attachments (
  id INTEGER PRIMARY KEY,
  message_id TEXT REFERENCES messages(id),
  part_idx INTEGER, filename TEXT, mime_type TEXT, size INTEGER
);
CREATE TABLE labels (id TEXT PRIMARY KEY, name TEXT);
CREATE VIRTUAL TABLE messages_fts USING fts5(
  subject, body_text, sender_addr, tokenize='porter'
);
-- sqlite-vec (created at runtime):
CREATE VIRTUAL TABLE vec_messages USING vec0(
  message_id TEXT, embedding float[1024]
);
CREATE TABLE chunks (
  chunk_id INTEGER PRIMARY KEY,
  message_id TEXT, seq INTEGER, text TEXT
);
```

Long bodies (> ~6k chars) are chunked for embedding; FTS5 always indexes the
full `body_text`. Every chunk carries `message_id` so hits resolve to emails.

### CLI contract

Rules (these exist for the agent layer — do not break them):

- Every command supports `--json`. Human output is the default; JSON is stable and documented.
- Bounded output: search/list default to top 10 with a `total` count field.
- Every result row carries `id` (the Proton internal ID) so agents can chain commands.
- Anything that writes outside the index supports `--dry-run` and prints the plan.
- Never interpret email content; attachment filenames are sanitized before path construction (reject `/`, `..`, control chars; collisions get `-1`, `-2` suffixes).
- Filenames in exports often start with `-`: always handle paths as `PathBuf`, never re-parse through a shell; support `--` separators.

Commands:

```
poste ingest <export-dir>        # incremental; skip IDs already present
poste search <query> [--from] [--to] [--after] [--before] [--label]
                 [--has-attachment] [--att-name substr] [--mode keyword|semantic|hybrid]
                 [--limit N] [--json]
poste show <id> [--headers] [--body] [--json]
poste attach list --from X [--ext pdf] [--json]
poste attach get  --from X --out DIR [--ext pdf] [--dry-run] [--json]
poste tag <id> <label> | poste tag list [--json]
poste embed [--backfill]         # embeddings via TabbyAPI; config from env/config file
poste stats                      # counts, date range, per-label, embed coverage
poste doctor                     # pairing check, orphan scan, db integrity
```

### Embeddings (deferred — see Phase 3)

- Model: `bge-m3` (1024-dim, 8K ctx) served by the existing TabbyAPI instance; endpoint/base URL/key from env (`POSTE_EMBED_URL` etc.) or `~/.config/poste/config.toml`.
- Embedding input: sanitized `subject + from + body_text` (or chunk).
- Hybrid search: run FTS5 BM25 and sqlite-vec KNN (top 50 each), fuse with RRF (`k=60`), return top N. ~30 lines, no deps.
- Embeddings are a cache, not a dependency: `search --mode keyword` must work with zero embeddings present, and the tool ships keyword-only.

## Phases

### Phase 0 — Skeleton + ingest (the spike) — **DONE**
- [x] `cargo new` in `~/coding/poste`; repo init; rustfmt/clippy defaults.
- [x] Schema creation + extension loading wired.
- [x] `ingest`: walk export dir, parse `metadata.json` + `.eml`, strip HTML, insert. Skip existing IDs. Handle leading-`-` filenames.
- [x] Fixture: synthetic export (3 messages, one with attachments, one `-`-prefixed ID) in `fixtures/export/` for tests.
- [x] Acceptance: full ingest of the July export → exactly 14,914 rows; re-ingest is a no-op; `doctor` clean.

### Phase 1 — Keyword search + show — **DONE**
- [x] `search --mode keyword` (FTS5), filters (from/to/after/before/label/has-attachment).
- [x] `total` count field in search output (CLI contract: bounded output *with* total).
- [x] `show`, `stats`.
- [x] Acceptance: 10 real queries you actually want answered return the right email on page 1. Keep a local, gitignored `eval/` notes file of these — it becomes the tuning set.

### Phase 2 — Attachments — **DONE**
- [x] Attachment inventory at ingest (done during Phase 0 — attachments table populated at ingest time).
- [x] `attach list` / `attach get` with `--dry-run`, sanitization, collision suffixes.
- [ ] Acceptance: "list PDFs from <accountant>" and a dry-run→real extraction into `~/notes/` both work via CLI alone. *(fixture-tested; run once against the live archive to close)*

### Phase 3 — Semantic + hybrid **(optional — deferred until keyword search proves insufficient)**

Embeddings are a *nice-to-have*, not a requirement: FTS5 covers exact/keyword
needs (receipts, names, attachments-from-X), and only "vague concept" recall
needs a model. Do not start this phase until real usage produces queries
keyword search cannot answer; record those in a local, gitignored `eval/`
notes file as the justification and tuning set. Schema already leaves room (chunks/vec tables
are additive), so deferring costs nothing.

- [ ] `embed` backfill against TabbyAPI (batched, resumable, `--backfill` finds un-embedded).
- [ ] `search --mode semantic` and default `--mode hybrid` (RRF).
- [ ] Acceptance: the recorded unsolvable queries now hit the right thread; hybrid ≥ keyword alone on the eval set.

### Phase 4 — Agent UX + hardening
- [ ] Skill file (opencode skill or AGENTS.md section) documenting verbs, JSON shape, dry-run pattern, and the hostile-content rule.
- [ ] Security pass: path traversal tests, fuzz-ish tests on malformed metadata/eml, `index.db` permissions check in `doctor`.
- [ ] Confirm `~/Documents/email-backup` is excluded from Syncthing/any sync.

### Phase 5 — Cadence + niceties (later)
- [ ] Dedup flow for new monthly exports (ingest is already incremental — just wire the reminder via paigateway + ntfy).
- [ ] Thread grouping via `References`/`ExternalID`.
- [ ] Optional: MCP wrapper over the same verbs; label write-back is **out of scope** (exports are read-only truth).

## Security constraints (carried from strategy doc)

- Everything under `~/Documents/email-backup`, chmod 700; `index.db` contains body text — never committed, never synced.
- No Proton credentials anywhere in this tool (it reads files, nothing more).
- Email content is hostile input: no panics on malformed data (use `thiserror`, never `unwrap` on parse paths), no content ever passed to a shell, attachment names sanitized.
- If Phase 3 is ever activated: embedding requests send body text to TabbyAPI — verify the endpoint is localhost-only and not Tailscale-exposed before enabling.

## Testing

- Unit: metadata parsing, HTML stripping, ID/filename handling, RRF fusion, path sanitization.
- Integration: fixture ingest → search → attach get, run against a temp dir.
- Property-ish: ingest twice == ingest once (row counts and content hashes stable).
- `cargo clippy --all-targets --all-features` + `cargo test` must pass before each phase closes.

## Open questions

1. Final name (poste is a placeholder).
2. Phase 3 only: does TabbyAPI serve an embedding model (bge-m3), or does it need loading? CPU fallback is fine at 15k emails.
3. `Flags` bitmask decode — deferred until a command needs it.
4. Where the skill file lives (repo skill dir vs project AGENTS.md) — decide in Phase 4.
