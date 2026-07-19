//! `wiki-mcp` — agent-wiki's MCP (Model Context Protocol) stdio server exposing
//! the wiki tools to agents.
//!
//! Each tool is a thin wrapper over `wiki_core::ops`, mirroring the `wiki` CLI
//! subcommands so both front-ends behave identically. Tool results are returned
//! as JSON text content so agents get structured, parseable output.
//!
//! The workspace root is taken from the `WIKI_WORKSPACE` environment variable,
//! else the process's current working directory. `wiki_init` is the exception —
//! it takes an explicit path and does not require an existing workspace.

use std::path::PathBuf;
use std::sync::Arc;

use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProtocolVersion, ServerCapabilities, ServerInfo,
};
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ErrorData as McpError, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio::sync::Mutex;

use wiki_core::{ops, EditMode, Workspace};

// --- tool parameter types -------------------------------------------------

#[derive(Debug, Deserialize, JsonSchema)]
struct InitParams {
    /// Directory to initialize as a wiki workspace.
    path: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchParams {
    /// Query string (English and/or CJK).
    query: String,
    /// Optional filter: `wiki`, `raw`, `entities`, `concepts`, `sources`, `overview`.
    #[serde(default)]
    filter: Option<String>,
    /// Maximum number of hits (default 10).
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadParams {
    /// Page id, e.g. `entities/ada` or `wiki/entities/ada.md`.
    page: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WriteParams {
    /// Page id, e.g. `entities/ada`.
    page: String,
    /// Markdown body (prose written by the agent; may contain `[[wikilinks]]`).
    body: String,
    /// Frontmatter `type` (entity|concept|source|…); inferred from folder if omitted.
    #[serde(default)]
    r#type: Option<String>,
    /// Page title.
    #[serde(default)]
    title: Option<String>,
    /// Tags.
    #[serde(default)]
    tags: Vec<String>,
    /// Raw sources this page draws from (e.g. `raw/article.md`).
    #[serde(default)]
    sources: Vec<String>,
    /// Upstream origin URL this page was derived from (e.g. a Feishu doc).
    #[serde(default)]
    origin: Option<String>,
    /// Editorial status (draft|stub|stable|needs-review).
    #[serde(default)]
    status: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EditParams {
    /// Page id.
    page: String,
    /// Heading of the section to patch.
    heading: String,
    /// Content to append or replace with.
    content: String,
    /// If true, replace the section body; otherwise append (default false).
    #[serde(default)]
    replace: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct BacklinksParams {
    /// Page id to find inbound links for.
    page: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct LogParams {
    /// Operation label for a new entry (e.g. `ingest`, `query`, `lint`). If
    /// omitted, the most recent entries are returned instead of appending.
    #[serde(default)]
    op: Option<String>,
    /// Title for a new entry.
    #[serde(default)]
    title: Option<String>,
    /// Optional detail body for a new entry.
    #[serde(default)]
    detail: Option<String>,
    /// When reading, how many recent entries to return (default 5).
    #[serde(default)]
    tail: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RawAddParams {
    /// Destination name under `raw/`, e.g. `article.md`.
    name: String,
    /// The full source content to store (immutable once written).
    content: String,
    /// Overwrite if it already exists (raw is normally immutable; default false).
    #[serde(default)]
    overwrite: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct RawReadParams {
    /// Raw source name, e.g. `article.md` or `raw/article.md`.
    name: String,
    /// Max characters per chunk (default 4000).
    #[serde(default)]
    max_chars: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PlanIngestParams {
    /// Raw source name, e.g. `feishu/Doc Title.md` or `raw/feishu/Doc Title.md`.
    raw: String,
    /// Maximum number of related pages to return (default 12).
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct QueryContextParams {
    /// User question or search query.
    query: String,
    /// Maximum number of search hits to retrieve (default 10).
    #[serde(default)]
    limit: Option<usize>,
    /// Maximum number of wiki pages to include with body excerpts (default 5).
    #[serde(default)]
    max_pages: Option<usize>,
    /// Maximum body characters per included page (default 2000).
    #[serde(default)]
    max_body_chars: Option<usize>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReviewParams {
    /// Number of recent log entries to include (default 5).
    #[serde(default)]
    recent_log: Option<usize>,
    /// Include a bounded git diff excerpt (default false).
    #[serde(default)]
    include_diff: bool,
    /// Maximum characters of diff to include when include_diff is true (default 4000).
    #[serde(default)]
    max_diff_chars: Option<usize>,
}

// --- server ----------------------------------------------------------------

#[derive(Clone)]
struct WikiServer {
    /// Default workspace root (from `WIKI_WORKSPACE` or cwd).
    default_root: PathBuf,
    /// Serializes tool operations. Tantivy allows only one `IndexWriter` per
    /// directory, and the MCP runtime dispatches tool calls concurrently, so we
    /// take this lock for the duration of every operation to avoid writer-lock
    /// contention. Wiki operations are fast, so serializing them is harmless.
    op_lock: Arc<Mutex<()>>,
    // Read by the code generated from `#[tool_handler]`; the dead-code lint
    // can't see that usage, so silence it here.
    #[allow(dead_code)]
    tool_router: ToolRouter<WikiServer>,
}

#[tool_router]
impl WikiServer {
    fn new(default_root: PathBuf) -> Self {
        Self {
            default_root,
            op_lock: Arc::new(Mutex::new(())),
            tool_router: Self::tool_router(),
        }
    }

    /// Open the configured workspace, mapping errors to MCP errors.
    fn ws(&self) -> Result<Workspace, McpError> {
        Workspace::open(&self.default_root).map_err(|e| {
            McpError::invalid_request(
                format!(
                    "no wiki workspace at {} ({e}); run wiki_init or set WIKI_WORKSPACE",
                    self.default_root.display()
                ),
                None,
            )
        })
    }

    #[tool(
        description = "Initialize a new wiki workspace (raw/, wiki/, AGENTS.md schema, git). Use once per knowledge base."
    )]
    async fn wiki_init(
        &self,
        Parameters(p): Parameters<InitParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let out = ops::init(&p.path).map_err(core_err)?;
        json_result(&out)
    }

    #[tool(
        description = "BM25 full-text search over wiki pages and raw sources (English + CJK). Returns ranked hits with path, category, title, score, and a snippet. Optional filter: wiki|raw|entities|concepts|sources|overview."
    )]
    async fn wiki_search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let hits = ops::search(&ws, &p.query, p.filter.as_deref(), p.limit.unwrap_or(10))
            .map_err(core_err)?;
        json_result(&hits)
    }

    #[tool(
        description = "Read a wiki page: returns its markdown body, frontmatter, outbound [[wikilinks]], and inbound backlinks."
    )]
    async fn wiki_read(
        &self,
        Parameters(p): Parameters<ReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let view = ops::read(&ws, &p.page).map_err(core_err)?;
        json_result(&view)
    }

    #[tool(
        description = "Create or overwrite a wiki page. Validates and manages frontmatter (created/updated dates, type inferred from folder), writes the file, and incrementally updates the search index. The agent supplies the prose."
    )]
    async fn wiki_write(
        &self,
        Parameters(p): Parameters<WriteParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let out = ops::write_page(
            &ws,
            ops::PageInput {
                path: p.page,
                kind: p.r#type,
                title: p.title,
                tags: p.tags,
                sources: p.sources,
                origin: p.origin,
                status: p.status,
                body: p.body,
            },
        )
        .map_err(core_err)?;
        json_result(&out)
    }

    #[tool(
        description = "Patch a page in place under a given heading — append to (default) or replace the section body. Re-indexes the page afterwards."
    )]
    async fn wiki_edit(
        &self,
        Parameters(p): Parameters<EditParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let mode = if p.replace {
            EditMode::Replace
        } else {
            EditMode::Append
        };
        let out = ops::edit_page(&ws, &p.page, &p.heading, &p.content, mode).map_err(core_err)?;
        json_result(&out)
    }

    #[tool(description = "List the wiki pages that link to a given page (its backlinks).")]
    async fn wiki_backlinks(
        &self,
        Parameters(p): Parameters<BacklinksParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let links = ops::backlinks(&ws, &p.page).map_err(core_err)?;
        json_result(&links)
    }

    #[tool(
        description = "Regenerate wiki/index.md (the catalog) from all page frontmatter, and rebuild the full search index. Run after a batch of edits."
    )]
    async fn wiki_index(&self) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let out = ops::reindex(&ws).map_err(core_err)?;
        json_result(&out)
    }

    #[tool(
        description = "Append an entry to wiki/log.md, or (if `op` is omitted) return the most recent entries. Entries are stamped `## [YYYY-MM-DD] <op> | <title>`."
    )]
    async fn wiki_log(
        &self,
        Parameters(p): Parameters<LogParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        match p.op {
            Some(op) => {
                let entry = ops::log_add(
                    &ws,
                    &op,
                    p.title.as_deref().unwrap_or(""),
                    p.detail.as_deref(),
                )
                .map_err(core_err)?;
                json_result(&entry)
            }
            None => {
                let entries = ops::log_tail(&ws, p.tail.unwrap_or(5)).map_err(core_err)?;
                json_result(&entries)
            }
        }
    }

    #[tool(
        description = "Structural health check of the wiki: dead links, orphan pages, stale pages, raw sources without a summary page, source pages missing an origin/sources provenance, and pages missing a title. Returns an issue list for the agent to fix."
    )]
    async fn wiki_lint(&self) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let report = ops::lint_wiki(&ws).map_err(core_err)?;
        json_result(&report)
    }

    #[tool(
        description = "Register a new immutable source under raw/ (the source of truth). Returns the stored path. Follow with raw_read to review it."
    )]
    async fn raw_add(
        &self,
        Parameters(p): Parameters<RawAddParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let out = ops::raw_add(&ws, &p.name, &p.content, p.overwrite).map_err(core_err)?;
        json_result(&out)
    }

    #[tool(
        description = "Read a registered raw source back in chunks (split on headings / a character budget) so the agent can review a long document section by section."
    )]
    async fn raw_read(
        &self,
        Parameters(p): Parameters<RawReadParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let chunks = ops::raw_read(&ws, &p.name, p.max_chars.unwrap_or(4000)).map_err(core_err)?;
        json_result(&chunks)
    }

    #[tool(
        description = "Plan an ingest before writing: inspect a raw source outline, suggest a sources/ page name, find likely affected wiki pages, and return semantic checks/next steps for the agent."
    )]
    async fn wiki_plan_ingest(
        &self,
        Parameters(p): Parameters<PlanIngestParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let plan = ops::plan_ingest(&ws, &p.raw, p.limit.unwrap_or(12)).map_err(core_err)?;
        json_result(&plan)
    }

    #[tool(
        description = "Build a bounded evidence packet for a query: ranked hits, selected page excerpts with frontmatter/links/backlinks, raw hits, and persistence guidance. Use before answering instead of reading the whole wiki."
    )]
    async fn wiki_query_context(
        &self,
        Parameters(p): Parameters<QueryContextParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let packet = ops::query_context(
            &ws,
            &p.query,
            p.limit.unwrap_or(10),
            p.max_pages.unwrap_or(5),
            p.max_body_chars.unwrap_or(2000),
        )
        .map_err(core_err)?;
        json_result(&packet)
    }

    #[tool(
        description = "Review wiki health before commit/review: run lint, include recent log entries, summarize git changed files, optionally include a bounded diff excerpt, and list semantic checks."
    )]
    async fn wiki_review(
        &self,
        Parameters(p): Parameters<ReviewParams>,
    ) -> Result<CallToolResult, McpError> {
        let _guard = self.op_lock.lock().await;
        let ws = self.ws()?;
        let review = ops::review(
            &ws,
            p.recent_log.unwrap_or(5),
            p.include_diff,
            p.max_diff_chars.unwrap_or(4000),
        )
        .map_err(core_err)?;
        json_result(&review)
    }
}

#[tool_handler]
impl ServerHandler for WikiServer {
    fn get_info(&self) -> ServerInfo {
        let mut implementation = Implementation::from_build_env();
        implementation.name = env!("CARGO_PKG_NAME").to_string();
        implementation.version = env!("CARGO_PKG_VERSION").to_string();
        implementation.title = Some("Agent Wiki MCP".to_string());
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(implementation)
            .with_protocol_version(ProtocolVersion::V_2024_11_05)
            .with_instructions(
                "Tools for maintaining a persistent, interlinked markdown wiki (the llm-wiki \
                 pattern). Prefer high-level tools when possible. Ingest: raw_add/fetch raw → \
                 wiki_plan_ingest → raw_read → wiki_write source summary → wiki_write/wiki_edit \
                 related entity/concept pages with [[links]] → wiki_index → wiki_log → wiki_review. \
                 Query: wiki_query_context → synthesize with citations → file durable findings back \
                 with wiki_write → wiki_log. Maintenance: wiki_review/wiki_lint → fix the issue list \
                 and semantic checks. The workspace's AGENTS.md holds the full conventions."
                    .to_string(),
            )
    }
}

/// Convert a `wiki_core` error into an MCP internal error.
fn core_err(e: wiki_core::WikiError) -> McpError {
    McpError::internal_error(e.to_string(), None)
}

/// Serialize any value to a JSON text tool result.
fn json_result<T: serde::Serialize>(value: &T) -> Result<CallToolResult, McpError> {
    let json = serde_json::to_string_pretty(value)
        .map_err(|e| McpError::internal_error(format!("serialization failed: {e}"), None))?;
    Ok(CallToolResult::success(vec![ContentBlock::text(json)]))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Log to stderr only — stdout is the MCP JSON-RPC channel.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();

    let default_root = std::env::var_os("WIKI_WORKSPACE")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

    tracing::info!(root = %default_root.display(), "starting agent-wiki MCP server");

    let service = WikiServer::new(default_root)
        .serve(stdio())
        .await
        .inspect_err(|e| tracing::error!("serving error: {e:?}"))?;

    service.waiting().await?;
    Ok(())
}
