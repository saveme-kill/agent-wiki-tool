# agent-wiki

[![CI](https://github.com/saveme-kill/agent-wiki-tool/actions/workflows/ci.yml/badge.svg)](https://github.com/saveme-kill/agent-wiki-tool/actions/workflows/ci.yml)
![Rust: 1.85+](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)

English | [简体中文](./README.zh-CN.md)

**agent-wiki** is a Rust implementation of the [**llm-wiki**](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f#file-llm-wiki-md) pattern ([local copy](./llm-wiki.md)): an **agent-maintained, persistent Markdown knowledge base** backed by Git, shipped with both a **`wiki` CLI** and a **`wiki-mcp` Model Context Protocol (MCP) server**.

Unlike classic RAG—where an LLM re-derives answers from raw document chunks from scratch on every question—here the agent **incrementally compiles and keeps current** a structured, densely interlinked wiki. Cross-references are already wired, contradictions are already flagged, and the synthesis reflects everything read so far.

- **Humans** curate sources, direct exploration, and ask questions.
- **Agents** own all the prose and synthesis—summaries, entity/concept pages, bidirectional `[[wikilinks]]`, and contradiction tracking.
- **agent-wiki** provides fast, deterministic, network-free **bookkeeping primitives**: BM25 + CJK full-text search, link/backlink graph resolution, section patching, `index.md` & `log.md` maintenance, structural `lint`, and bounded `plan-ingest` / `query-context` / `review` packets.

---

## Highlights

- **Dual Front-Ends, Identical Behavior:** `wiki` (CLI) and `wiki-mcp` (MCP stdio server) are thin wrappers over the same `wiki-core::ops` library and emit identical JSON schemas.
- **Obsidian-Compatible Plain Files:** Standard Markdown files with YAML frontmatter (`type`, `title`, `tags`, `sources`, `origin`, `created`, `updated`, `status`) and `[[wikilinks]]`—browse live in Obsidian Graph View and Dataview while your agent edits.
- **Embedded English + CJK Search:** Powered by Tantivy 0.26 BM25 with a built-in character n-gram analyzer (`"cjk"`) for space-free Chinese/Japanese/Korean recall alongside English—zero external tokenizers or vector databases required.
- **Context-Efficient Agent Packets:** High-level `plan-ingest`, `query-context`, and `review` operations return bounded outlines, excerpts, backlinks, and git diffs so agents never need to dump the entire wiki into context.
- **Zero-Network Core & Git-Native Storage:** `wiki-core` has no HTTP dependencies and confines all paths inside the workspace. Every wiki workspace is a self-contained Git repository.

---

## Installation

### Prebuilt Binaries

Download precompiled archives (`wiki` + `wiki-mcp`) for Linux (`x86_64`), macOS (`x86_64` / Apple Silicon `aarch64`), and Windows (`x86_64`) from [GitHub Releases](https://github.com/saveme-kill/agent-wiki-tool/releases).

### Install via Cargo

Requires Rust `1.85+`:

```sh
cargo install --git https://github.com/saveme-kill/agent-wiki-tool wiki-cli wiki-mcp
```

### Build from Source

```sh
git clone https://github.com/saveme-kill/agent-wiki-tool.git
cd agent-wiki-tool
cargo build --workspace --release
# Produces target/release/wiki and target/release/wiki-mcp (or .exe on Windows)
```

---

## Workspace Layout

`wiki init` scaffolds a self-contained Git repository with three layers:

```text
workspace/
  AGENTS.md            # Schema: conventions + Ingest / Query / Lint operating manual
  raw/                 # Immutable raw source documents (read-only source of truth)
  wiki/
    overview.md        # Top-level synthesis / evolving thesis
    index.md           # Content catalog: link + one-line summary per page, by category
    log.md             # Append-only chronological operation log
    entities/          # People, organizations, systems, products
    concepts/          # Ideas, methods, architectures, recurring themes
    sources/           # One summary page per ingested raw source
  .wiki/index/         # Tantivy full-text index (gitignored, rebuildable cache)
  .gitignore
```

First-class provenance is tracked via `sources` (paths under `raw/`) and `origin` (upstream URL, e.g., a web article or Feishu doc). `wiki read` surfaces `origin`, and `wiki lint` flags `type: source` pages missing both `origin` and `sources`.

---

## Quickstart (CLI)

```sh
# 1. Initialize a wiki workspace (creates layout + best-effort git init)
wiki init ./my-wiki

# 2. Register an immutable raw source and inspect it
wiki -C ./my-wiki add-raw article.md
wiki -C ./my-wiki plan-ingest raw/article.md
wiki -C ./my-wiki read-raw article.md

# 3. Write & cross-link wiki pages
wiki -C ./my-wiki write entities/ada \
    --title "Ada Lovelace" --type entity \
    --source raw/article.md \
    --body 'First programmer; collaborated on [[concepts/analytical-engine]].'

# 4. Patch an existing heading section
wiki -C ./my-wiki edit concepts/analytical-engine "Notes" --content "Referenced by [[entities/ada]]."

# 5. Rebuild catalog & query
wiki -C ./my-wiki index
wiki -C ./my-wiki search "programmer"
wiki -C ./my-wiki search "计算" --filter concepts
wiki -C ./my-wiki query-context "analytical engine"
wiki -C ./my-wiki read entities/ada
wiki -C ./my-wiki backlinks concepts/analytical-engine

# 6. Record log & run health/review checks
wiki -C ./my-wiki log --op ingest --title "Ada article"
wiki -C ./my-wiki log --tail 5
wiki -C ./my-wiki lint
wiki -C ./my-wiki review --diff
```

Pass `--json` to any CLI command to emit machine-readable JSON identical to the MCP server output.

---

## MCP Server (`wiki-mcp`)

`wiki-mcp` exposes all 14 operations over stdio using the Model Context Protocol:
`wiki_init`, `wiki_search`, `wiki_read`, `wiki_write`, `wiki_edit`, `wiki_backlinks`, `wiki_index`, `wiki_log`, `wiki_lint`, `raw_add`, `raw_read`, `wiki_plan_ingest`, `wiki_query_context`, and `wiki_review`.

The target workspace is read from `WIKI_WORKSPACE` (defaults to the process working directory). Diagnostic logs go strictly to `stderr` (`RUST_LOG=info` by default).

### MCP Client Configuration

Add `wiki-mcp` to Claude Code, Claude Desktop, Cursor, or any MCP-compatible client:

```json
{
  "mcpServers": {
    "wiki": {
      "command": "/absolute/path/to/wiki-mcp",
      "env": {
        "WIKI_WORKSPACE": "/absolute/path/to/my-wiki"
      }
    }
  }
}
```

---

## Command & Tool Reference

### Workspace & Source Layer

| CLI | MCP Tool | Key Parameters | Purpose | Typical Timing |
|---|---|---|---|---|
| `init [path]` | `wiki_init` | `path` | Scaffold `AGENTS.md`, `raw/`, `wiki/`, `.wiki/index/`, `.gitignore`, and best-effort `git init` | First-time setup of a knowledge base |
| `add-raw <file>` | `raw_add` | CLI: `file`, `--name`, `--overwrite`; MCP: `name`, `content`, `overwrite` | Register a source into `raw/` (refuses overwrite by default) and incrementally index it | Adding a local Markdown/text document |
| `read-raw <name>` | `raw_read` | `name`, `--max-chars` / `max_chars` | Split a raw source into heading-aware chunks within a character budget | Reading long sources without overflowing context |
| `plan-ingest <raw>` | `wiki_plan_ingest` | `raw`, `--limit` / `limit` | Extract raw outline, suggest a `sources/` page, search likely affected wiki pages, and list semantic checks | Before writing source/entity/concept pages |

### Query & Navigation

| CLI | MCP Tool | Key Parameters | Purpose | Typical Timing |
|---|---|---|---|---|
| `query-context <query>` | `wiki_query_context` | `query`, `--limit`, `--max-pages`, `--max-body-chars` | Build a bounded evidence packet: hits, excerpts, frontmatter, outbound links, backlinks, and raw hits | Primary entry point when answering user questions |
| `search <query>` | `wiki_search` | `query`, `--filter`, `--limit` | BM25 search across wiki pages & raw sources; supports `wiki`, `raw`, `entities`, `concepts`, `sources`, `overview` | Targeted follow-up lookup or manual debugging |
| `read <page>` | `wiki_read` | `page` | Read full page body, frontmatter, outbound `[[wikilinks]]`, and backlinks | Drilling into a specific page when excerpts aren't enough |
| `backlinks <page>` | `wiki_backlinks` | `page` | List all wiki pages linking to the target page | Assessing impact radius and ensuring bidirectional links |

### Write & Maintenance

| CLI | MCP Tool | Key Parameters | Purpose | Typical Timing |
|---|---|---|---|---|
| `write <page>` | `wiki_write` | `page`, `body`, `type`, `title`, `tags`, `sources`, `origin`, `status` | Create or overwrite a wiki page, maintain dates/frontmatter, and incrementally index | Creating source summaries, entity/concept pages, or filing query syntheses |
| `edit <page> <heading>` | `wiki_edit` | `page`, `heading`, `content`, `replace` | Append to or replace an *existing* heading section and incrementally index | Localized updates to an existing page |
| `index` | `wiki_index` | *(none)* | Regenerate `wiki/index.md` and perform a full search-index rebuild | Final calibration step after a batch of writes/edits |
| `log` | `wiki_log` | Write: `op`, `title`, `detail`; Read: `tail` | Append to or tail `wiki/log.md` (`## [YYYY-MM-DD] <op> \| <title>`) | Recording `ingest`, `query`, `lint`, or `review` milestones |
| `lint` | `wiki_lint` | *(none)* | Structural health check: dead links, orphans, stale pages, unindexed raw files, missing `origin`/`sources`, missing titles | Periodic maintenance or pre-commit check |
| `review` | `wiki_review` | `--recent-log`, `--diff`, `--max-diff-chars` | Combine `lint`, recent log entries, git changed files, optional diff excerpt, and semantic review checklist | Final verification before handing off or committing |

---

## Core Workflows

Every initialized workspace includes a customizable `AGENTS.md` operating manual guiding the agent through three loops:

1. **Ingest:** `add-raw` → `plan-ingest` → `read-raw` → align key takeaways → `write` a `sources/` summary page → `write`/`edit` 10–15 related `entities/` and `concepts/` pages with bidirectional `[[wikilinks]]` → `index` → `log` → `review`.
2. **Query:** `query-context` → optional `read` / `backlinks` drill-down → synthesize cited answer → file valuable analyses or comparisons back into `wiki/` via `write` → `log`.
3. **Review & Lint:** `review` / `lint` → resolve dead links, link orphans, refresh stale claims, backfill missing `origin`/`sources`, and verify cross-document contradictions.

### Key Operational Semantics

- **Incremental indexing on writes:** `write`, `edit`, and `add-raw` automatically update the Tantivy index (`reindexed: true`). Run `index` once at the end of a batch to regenerate `wiki/index.md`.
- **Lazy initial search index:** Running `search` or `query-context` on a fresh workspace automatically builds the index on first use.
- **Section-safe `edit`:** `edit` patches an existing Markdown heading (`heading '...' not found` if absent). Create new page structures with `write`, then maintain sections with `edit`.

---

## External Document Bridges (`scripts/`)

`wiki-core` intentionally stays 100% offline and reads only local UTF-8 files. Authenticated online documents (such as Feishu / Lark docs) are fetched at the orchestration boundary via thin bridge scripts under [`scripts/`](./scripts/README.md):

- [`scripts/feishu-ingest.sh`](./scripts/feishu-ingest.sh) — Fetch a single Feishu document via `lark-cli`, stamp `<!-- origin -->` & `<!-- fetched -->` metadata comments, and register it under `raw/feishu/`.
- [`scripts/import-lark-docs.py`](./scripts/import-lark-docs.py) — Batch-import a Markdown manifest table of Feishu documents into `raw/feishu/` along with hierarchical `status: stub` source pages.

See [`scripts/README.md`](./scripts/README.md) for prerequisites and usage examples.

---

## Architecture

```text
crates/
  wiki-core/   # Pure domain library + `ops` orchestration + all unit tests (zero protocol/HTTP deps)
  wiki-cli/    # `wiki` binary (clap) — thin CLI wrapper over `wiki_core::ops`
  wiki-mcp/    # `wiki-mcp` binary (rmcp 2.1) — thin MCP stdio server over `wiki_core::ops`
```

---

## Development

```sh
cargo test --workspace                          # Run unit tests
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo doc --workspace --no-deps
```

See [`AGENTS.md`](./AGENTS.md) for detailed architecture rules, Tantivy/MCP concurrency invariants, and coding conventions.
