# AGENTS.md

Repo-level guide for agents working on **agent-wiki**. (Note: the file at
`crates/wiki-core/src/templates/AGENTS.md` is unrelated — it is a *source asset*
embedded via `include_str!` and written into generated wiki workspaces as the
end-user's schema. Do not confuse the two; edits here do not affect that template.)

## 1. Project Overview

agent-wiki is a Rust implementation of the **llm-wiki pattern** (see `llm-wiki.md`):
instead of re-deriving answers from raw documents on every query (classic RAG), an
agent incrementally builds and maintains a persistent, interlinked markdown wiki
stored as plain files under git. The tools here are deterministic *bookkeeping*
primitives (search, link graph, index/log maintenance, lint); the agent writes all
prose.

It ships as two front-ends over one shared library:

- **`wiki-core`** (`crates/wiki-core`) — all domain logic, no protocol/IO-framework
  deps. Modules: `workspace` (three-layer scaffold, page CRUD, git init, path
  safety, section edits), `page` (YAML frontmatter parse/render), `link`
  (`[[wikilink]]` extraction + forward/back link graph), `index` (regenerate
  `index.md`), `logbook` (append/grep `log.md`), `search` (tantivy BM25 + CJK),
  `ingest` (raw-source chunking), `lint` (structural checks), and `ops` (the
  orchestration layer, including the higher-level plan/query-context/review
  packets used by agents).
- **`wiki-cli`** (`crates/wiki-cli`) — the `wiki` binary (clap).
- **`wiki-mcp`** (`crates/wiki-mcp`) — the `wiki-mcp` MCP stdio server (rmcp 2.1).

**Critical architectural rule:** both front-ends are *thin* wrappers that parse
their own inputs and call `wiki_core::ops::*`. All multi-step orchestration
(e.g. write-then-reindex, lazy index build) lives in `ops.rs`. This is what
guarantees the CLI and MCP server behave identically — **put new cross-cutting
logic in `wiki-core` (usually `ops.rs`), never duplicate it across the two
front-ends.**

A "workspace" is a user's wiki directory (git repo) with three layers: `raw/`
(immutable sources), `wiki/` (agent-written pages + `overview.md`/`index.md`/
`log.md`), and `.wiki/index/` (tantivy index, gitignored). Do not confuse the
*build workspace* (this Cargo workspace) with a *wiki workspace* (user data).

## 2. Build & Commands

```sh
cargo build --workspace                 # debug build of all three crates
cargo build --workspace --release       # release; produces target/release/{wiki,wiki-mcp}
cargo test --workspace                  # run all tests (49 unit tests, all in wiki-core)
cargo test -p wiki-core <filter>        # run a subset, e.g. `cargo test -p wiki-core search`
cargo clippy --workspace                # lint; repo is expected to be clippy-clean
cargo fmt                               # format (standard rustfmt, no custom config)
cargo run -p wiki-cli -- <args>         # run the CLI, e.g. `-- init ./my-wiki`
```

**Network / sandbox note:** first build fetches crates (tantivy pulls a large tree)
and needs network access. In a restricted sandbox `cargo build/test` fails with
"Operation not permitted" writing to `~/.cargo/registry`; run those commands with
the sandbox disabled. Pure editing/searching does not need this.

GitHub Actions CI and release workflows live under `.github/workflows/{ci,release}.yml`.
There is no `rust-toolchain.toml` and no custom `rustfmt.toml`/`clippy.toml` in this
repo (`rust-version = "1.85"` in `Cargo.toml`).

## 3. Code Style

- **Edition 2021.** Dependencies are pinned once in the root `[workspace.dependencies]`;
  member crates reference them with `<dep>.workspace = true`. Add new shared deps
  there, not per-crate.
- **Errors:** `wiki-core` defines `WikiError` (thiserror) and `type Result<T>`.
  Prefer these over `anyhow` inside the library. Use `WikiError::io(path, e)` to
  attach a path to `io::Error`. Third-party errors get `From` impls in `error.rs`
  (this is how tantivy's several error types are funneled into `WikiError::Search`).
  The two front-end `main.rs` files use `anyhow` at the boundary.
- **Serialization:** result/output structs derive `serde::Serialize` and live in
  `wiki-core` so both front-ends emit identical shapes. Frontmatter uses
  `#[serde(rename = "type")]` for the `kind` field and `#[serde(flatten)] extra`
  to round-trip unknown YAML keys — preserve this.
- **tantivy 0.26 is version-sensitive.** Its API changed a lot across releases.
  This code relies on: `TantivyDocument`, `ContentBlock`-free reads via the
  `Value` trait's `.as_str()`, `TopDocs::with_limit(n).order_by_score()` (bare
  `with_limit` is not a `Collector`), `writer.delete_all_documents()`,
  `ReloadPolicy::OnCommitWithDelay`, and tokenizers that must be **re-registered
  after every `open_in_dir`/`create_in_dir`** (they are not persisted). Don't
  "modernize" these against newer/older docs without checking `search.rs`.
- **rmcp 2.1 tool pattern:** `#[tool_router]` on the impl, `#[tool]` per method,
  `#[tool_handler]` on the `ServerHandler` impl, a `tool_router: ToolRouter<Self>`
  field, params as `Parameters<T>` where `T: Deserialize + JsonSchema`, results via
  `ContentBlock::text(...)`. Keep each tool a one-liner over an `ops::*` call.
- Comments explain *why* (esp. the tantivy/rmcp gotchas and concurrency notes);
  match the existing density. Keep modules focused and unit-tested in-file.

## 4. Testing

- **Framework:** standard `cargo test`. All 49 tests are `#[cfg(test)] mod tests`
  blocks colocated in each `wiki-core` source file. `wiki-cli` and `wiki-mcp` have
  no unit tests — logic to test belongs in `wiki-core`, so add tests there.
- **Fixtures:** tests use the `tempfile` dev-dependency and a local `init_ws()`
  helper that scaffolds a throwaway workspace per test. Follow that pattern; never
  write test artifacts into the repo tree.
- **Coverage focus:** page frontmatter round-trip, wikilink extraction (incl.
  code-block exclusion and CJK targets), link resolution/backlinks, section edits,
  index grouping, log append/parse, ingest chunking, lint checks, and search
  (English + CJK + filters). `search::tests::reindex_is_safe_while_handle_is_open`
  is a **regression test** for the MCP-process concurrency bug — keep it green.
- Run `cargo test --workspace` before finishing changes; also `cargo clippy
  --workspace`. For end-to-end MCP checks, drive `wiki-mcp` over stdio with a
  **sequential** JSON-RPC client (initialize → notifications/initialized →
  tools/call one at a time); piping many requests at once races because tool calls
  dispatch concurrently.

## 5. Security

- **Path traversal is guarded:** `Workspace::abs` rejects any path containing `..`
  and everything is rooted under the workspace dir. Route all file access through
  `Workspace` helpers (`abs`/`rel`/`read_file`/`write_page`); do not build paths
  ad hoc or accept absolute paths from tool input.
- **`raw/` is immutable** by contract (the source of truth). `raw_add` refuses to
  overwrite existing sources unless `overwrite` is explicitly set. Preserve this;
  the wiki layer is the only agent-writable layer.
- **MCP stdout is the JSON-RPC channel.** `wiki-mcp` logs only to **stderr**
  (`tracing_subscriber` with `.with_writer(std::io::stderr)`). Never `println!`/
  write to stdout from the server or you corrupt the protocol stream.
- **tantivy allows one `IndexWriter` per directory.** The MCP runtime dispatches
  tool calls concurrently, so `WikiServer` serializes every tool via an
  `Arc<Mutex<()>>` `op_lock`. Any new tool handler must also take this lock;
  dropping it reintroduces `LockBusy` failures.
- No secrets, credentials, or network calls are handled by this code. The `.wiki/`
  search index is derived/rebuildable and gitignored — treat it as a cache.

## 6. Configuration

- **`WIKI_WORKSPACE`** (env) — the workspace root used by `wiki-mcp` (defaults to
  the process CWD). Set it in the MCP client config's `env`.
- **`-C, --workspace <path>`** (CLI, global) — the workspace root for `wiki`
  (defaults to `.`). **`--json`** (global) switches any command to machine-readable
  output; both front-ends emit the same serde-serialized structs.
- **`RUST_LOG`** — respected by the server's `tracing_subscriber` env-filter
  (defaults to `info`).
- Each wiki workspace is self-contained: `AGENTS.md` (the end-user schema, from the
  embedded template), `.gitignore` (ignores `.wiki/`), and git init happen in
  `Workspace::init`. git init/commit is best-effort — code must not assume git is
  present or configured.
- MCP client registration example:

```json
{
  "mcpServers": {
    "wiki": {
      "command": "/absolute/path/to/target/release/wiki-mcp",
      "env": { "WIKI_WORKSPACE": "/absolute/path/to/my-wiki" }
    }
  }
}
```
