# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**agent-wiki** is a Rust implementation of the llm-wiki pattern: an agent-maintained, persistent markdown wiki stored as plain files under git. Two thin front-ends (`wiki` CLI, `wiki-mcp` MCP server) share one domain library (`wiki-core`). The tools provide deterministic bookkeeping (search, link graph, index/log, lint); the agent writes all prose.

Also see `AGENTS.md` at the repo root — it is the detailed developer guide. The file `crates/wiki-core/src/templates/AGENTS.md` is an embedded template written into *generated* wiki workspaces; edits to `AGENTS.md` do not affect that template (and vice versa).

## Build & Commands

```sh
cargo build --workspace                 # debug build (all 3 crates)
cargo build --workspace --release       # release → target/release/{wiki,wiki-mcp}
cargo test --workspace                  # run all 49 unit tests (all in wiki-core)
cargo test -p wiki-core <filter>        # run subset by name filter (e.g. `search`)
cargo test -p wiki-core --test <name>   # (no integration tests exist yet)
cargo clippy --workspace                # repo is expected to be clippy-clean
cargo fmt                               # standard rustfmt; no rustfmt.toml
cargo run -p wiki-cli -- <args>         # e.g. `-- init ./my-wiki`
```

No `rust-toolchain.toml`, `clippy.toml`, or `rustfmt.toml` exist. Development toolchain: rustc/cargo ~1.95.

**Sandbox note:** first `cargo build/test` needs network to fetch crates (tantivy pulls a large tree). In restricted sandboxes those commands fail writing to `~/.cargo/registry`.

## Workspace Architecture

Cargo workspace with three crates. The critical invariant: **both front-ends call the same `wiki_core::ops::*` functions** — all multi-step orchestration lives there. Never duplicate logic across `wiki-cli` and `wiki-mcp`.

```
crates/
  wiki-core/    # Domain logic (no protocol/IO deps). ALL tests live here.
    src/
      error.rs      # WikiError (thiserror) + From impls for tantivy/etc.
      page.rs       # YAML frontmatter parse/render + markdown page model
      workspace.rs  # 3-layer scaffold, page CRUD, path safety, section edits, git init
      link.rs       # [[wikilink]] extraction + forward/back link graph
      search.rs     # tantivy 0.26 BM25 + character n-gram CJK tokenizer
      index.rs      # rebuild index.md (catalog of pages with summaries)
      logbook.rs    # append-only log.md with parse/grep
      ingest.rs     # raw-source chunking (add-raw, read-raw)
      lint.rs       # structural checks: dead links, orphans, stale, missing origin, etc.
      ops.rs        # ★ ORCHESTRATION LAYER — all entry points the front-ends call
      templates/    # AGENTS.md + .gitignore embedded via include_str!
  wiki-cli/     # Thin clap wrapper around ops::* with --json output
    src/main.rs
  wiki-mcp/     # Thin rmcp 2.1 stdio server, same ops::* calls
    src/main.rs
```

When adding a new feature, the flow is:

1. Add the pure logic and unit tests in the relevant `wiki-core` module (e.g., new lint check in `lint.rs`).
2. Wire it into `ops.rs` if it needs orchestration with other ops (e.g., write → reindex).
3. Add a clap subcommand in `wiki-cli/src/main.rs` and an `#[tool]` handler in `wiki-mcp/src/main.rs` — both one-liners calling the same `ops::*` function.

## Key Gotchas

**tantivy 0.26 API fragility** (`search.rs`):
- API changed significantly across releases — do not "modernize" against newer/older docs.
- Relies on: `TantivyDocument`, `Value` trait `.as_str()` reads, `TopDocs::with_limit(n).order_by_score()`, `writer.delete_all_documents()`, `ReloadPolicy::OnCommitWithDelay`.
- **Tokenizers are NOT persisted** — must re-register the custom `"cjk"` n-gram tokenizer after every `Index::open_in_dir` / `create_in_dir`.
- Only one `IndexWriter` per directory. The MCP server wraps every tool in an `Arc<Mutex<()>>` (`op_lock`) because rmcp dispatches tool calls concurrently. Any new MCP tool handler MUST take this lock to avoid `LockBusy` failures.

**Path safety** (`workspace.rs`):
- All file access goes through `Workspace::abs`/`rel`/`read_file`/`write_page` helpers.
- `Workspace::abs` rejects any path containing `..`. Never build filesystem paths ad hoc or accept absolute paths from tool input.

**MCP stdout is the protocol channel** (`wiki-mcp`):
- All logging goes to stderr via `tracing_subscriber::with_writer(std::io::stderr)`.
- **Never `println!` or write to stdout** from the MCP server — it corrupts the JSON-RPC stream.
- `rmcp` 2.1 pattern: `#[tool_router]` on impl, `#[tool]` per method, `#[tool_handler]` on `ServerHandler`, params as `Parameters<T>` where `T: Deserialize + JsonSchema`, results as `ContentBlock::text(...)`.

**`raw/` is immutable** by contract — the source of truth. `raw_add` refuses overwrites without explicit `overwrite: true`.

## Testing Patterns

- All tests are `#[cfg(test)] mod tests` blocks colocated in each `wiki-core` source file. `wiki-cli` and `wiki-mcp` have no tests.
- Fixtures use `tempfile` and a local `init_ws()` helper that scaffolds a throwaway workspace per test. Follow this pattern — never write test artifacts into the repo.
- Coverage focus areas: frontmatter round-trip, wikilink extraction (code-block exclusion, CJK targets), link resolution/backlinks, section edits, index grouping, log append/parse, ingest chunking, lint checks, search (English + CJK + filters).
- `search::tests::reindex_is_safe_while_handle_is_open` is a regression test for the MCP concurrency bug — keep it green.
- End-to-end MCP tests: drive `wiki-mcp` over stdio with a **sequential** JSON-RPC client (initialize → initialized notification → tools/call one at a time).

## Code Style Conventions

- Edition 2021. Shared deps pinned in root `[workspace.dependencies]`; member crates use `<dep>.workspace = true`.
- `WikiError` / `Result<T>` throughout `wiki-core`; prefer `WikiError::io(path, e)` to attach paths to I/O errors. `anyhow` only at the two front-end `main.rs` boundaries.
- Output structs derive `serde::Serialize` and live in `wiki-core` so both front-ends emit identical JSON.
- Frontmatter uses `#[serde(rename = "type")]` for the `kind` field and `#[serde(flatten)] extra: BTreeMap` to round-trip unknown YAML keys — preserve unknown keys on write.
- Comments explain *why*, especially tantivy/rmcp gotchas and concurrency notes — match existing density.

## Configuration & Environment

- **`WIKI_WORKSPACE`** env: workspace root for `wiki-mcp` (defaults to CWD). Set in MCP client `env`.
- **`-C, --workspace <path>`**: CLI global flag (defaults to `.`).
- **`--json`**: CLI global flag for machine-readable output (same structs as MCP results).
- **`RUST_LOG`**: respected by `tracing_subscriber` on the MCP server (default: `info`).
- git init/commit in `Workspace::init` is best-effort — code must not assume git is present.
- MCP client config:
  ```json
  { "mcpServers": { "wiki": {
      "command": "/abs/path/to/wiki-mcp",
      "env": { "WIKI_WORKSPACE": "/abs/path/to/my-wiki" } } } }
  ```

## Useful References

- `llm-wiki.md` at repo root — the original pattern document by Karpathy.
- `scripts/feishu-ingest.sh` — single-doc bridge using `lark-cli` to fetch Feishu docs into `raw/feishu/` (the Rust core has zero network deps).
- `scripts/import-lark-docs.py` — bulk importer that parses a markdown manifest table of Feishu doc tokens and writes them + stub source pages.
