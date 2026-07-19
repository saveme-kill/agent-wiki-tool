//! wiki-core — domain logic for an agent-maintained, persistent markdown wiki.
//!
//! This crate implements the "llm-wiki" pattern: instead of re-deriving answers
//! from raw documents on every query (classic RAG), an agent incrementally
//! builds and maintains a structured, interlinked collection of markdown pages.
//! The wiki is a compounding artifact — cross-references, summaries and
//! syntheses are written once and kept current.
//!
//! `wiki-core` deliberately contains **no** protocol/IO glue (no MCP, no clap).
//! It exposes deterministic *bookkeeping* primitives — search, the link graph,
//! index/log maintenance, lint checks, source ingestion — that the two
//! front-ends (`wiki-cli`, `wiki-mcp`) wrap thinly so both behave identically.
//!
//! The agent owns the prose (page text, synthesis, judgement); the tools here
//! only do the mechanical navigation and consistency work.

pub mod error;
pub mod index;
pub mod ingest;
pub mod link;
pub mod lint;
pub mod logbook;
pub mod ops;
pub mod page;
pub mod search;
pub mod workspace;

pub use error::{Result, WikiError};
pub use index::rebuild_index;
pub use ingest::{raw_add as ingest_raw_add, raw_read as ingest_raw_read, Chunk};
pub use link::{extract_wikilinks, LinkGraph, WikiLink};
pub use lint::{lint, LintIssue, LintKind, LintReport};
pub use logbook::{log_append, log_recent, LogEntry};
pub use ops::{
    backlinks, edit_page, init, lint_wiki, log_add, log_tail, plan_ingest, query_context, raw_add,
    raw_add_from_file, raw_read, read, reindex, review, search, write_page, GitChangedFile,
    GitReview, IndexOutcome, IngestPlanOutcome, InitOutcome, PageEvidence, PageInput,
    QueryContextOutcome, RawAddOutcome, RawOutline, RelatedPage, ReviewOutcome, SourcePagePlan,
    WriteOutcome,
};
pub use page::{Frontmatter, Page, PageView};
pub use search::{SearchHit, SearchIndex};
pub use workspace::{EditMode, Workspace};
