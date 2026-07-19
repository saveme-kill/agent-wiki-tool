//! `wiki` — command-line front-end for the agent-maintained markdown wiki.
//!
//! A thin wrapper over `wiki_core::ops`: every subcommand parses its inputs,
//! calls the corresponding core operation, and prints the result (human-readable
//! by default, or JSON with `--json` for scripting). The MCP server wraps the
//! same core functions, so the two front-ends behave identically.

use std::io::Read;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use wiki_core::ops;
use wiki_core::{EditMode, Workspace};

/// Agent-maintained, persistent markdown wiki (llm-wiki pattern).
#[derive(Parser)]
#[command(name = "wiki", version, about, long_about = None)]
struct Cli {
    /// Path to the wiki workspace (defaults to the current directory).
    #[arg(long, short = 'C', global = true, default_value = ".")]
    workspace: String,

    /// Emit machine-readable JSON instead of human-readable text.
    #[arg(long, global = true)]
    json: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Scaffold a new workspace (raw/, wiki/, AGENTS.md, git init).
    Init {
        /// Directory to initialize (defaults to --workspace).
        path: Option<String>,
    },
    /// BM25 search over wiki pages and raw sources (English + CJK).
    Search(SearchArgs),
    /// Read a page: body, frontmatter, outbound links, backlinks.
    Read {
        /// Page id, e.g. `entities/ada` or `wiki/entities/ada.md`.
        page: String,
    },
    /// Create or overwrite a page (body read from --body or stdin).
    Write(WriteArgs),
    /// Patch a page under a heading (append or replace).
    Edit(EditArgs),
    /// List pages that link to a page.
    Backlinks { page: String },
    /// Regenerate index.md and rebuild the search index.
    Index,
    /// Append a log entry, or show the most recent entries.
    Log(LogArgs),
    /// Structural health check (dead links, orphans, stale, …).
    Lint,
    /// Register a raw source file into raw/.
    AddRaw(AddRawArgs),
    /// Read a registered raw source back in chunks.
    ReadRaw(ReadRawArgs),
    /// Plan an ingest before writing wiki pages.
    PlanIngest(PlanIngestArgs),
    /// Build a bounded evidence packet for a query.
    QueryContext(QueryContextArgs),
    /// Review lint, recent log, and git changes before commit/review.
    Review(ReviewArgs),
}

#[derive(Args)]
struct SearchArgs {
    /// Query string.
    query: String,
    /// Restrict to `wiki`, `raw`, `entities`, `concepts`, `sources`, or `overview`.
    #[arg(long)]
    filter: Option<String>,
    /// Maximum number of hits.
    #[arg(long, default_value_t = 10)]
    limit: usize,
}

#[derive(Args)]
struct WriteArgs {
    /// Page id, e.g. `entities/ada`.
    page: String,
    /// Page body markdown. If omitted, read from stdin.
    #[arg(long)]
    body: Option<String>,
    /// Frontmatter `type` (entity|concept|source|…); inferred from folder if omitted.
    #[arg(long = "type")]
    kind: Option<String>,
    /// Page title.
    #[arg(long)]
    title: Option<String>,
    /// Tags (repeatable).
    #[arg(long)]
    tag: Vec<String>,
    /// Raw sources this page draws from (repeatable).
    #[arg(long)]
    source: Vec<String>,
    /// Upstream origin URL this page was derived from (e.g. a Feishu doc).
    #[arg(long)]
    origin: Option<String>,
    /// Editorial status (draft|stub|stable|needs-review).
    #[arg(long)]
    status: Option<String>,
}

#[derive(Args)]
struct EditArgs {
    /// Page id.
    page: String,
    /// Heading of the section to patch.
    heading: String,
    /// Content to append/replace. If omitted, read from stdin.
    #[arg(long)]
    content: Option<String>,
    /// Replace the section body instead of appending.
    #[arg(long)]
    replace: bool,
}

#[derive(Args)]
struct LogArgs {
    /// Operation label for a new entry (e.g. ingest|query|lint). Omit to read.
    #[arg(long)]
    op: Option<String>,
    /// Title for a new entry.
    #[arg(long)]
    title: Option<String>,
    /// Optional detail body for a new entry.
    #[arg(long)]
    detail: Option<String>,
    /// When reading, how many recent entries to show.
    #[arg(long, default_value_t = 5)]
    tail: usize,
}

#[derive(Args)]
struct AddRawArgs {
    /// Path to a local file to register as a raw source.
    file: String,
    /// Destination name under raw/ (defaults to the file's basename).
    #[arg(long)]
    name: Option<String>,
    /// Overwrite if the raw source already exists (raw is normally immutable).
    #[arg(long)]
    overwrite: bool,
}

#[derive(Args)]
struct ReadRawArgs {
    /// Raw source name, e.g. `article.md` or `raw/article.md`.
    name: String,
    /// Max characters per chunk.
    #[arg(long, default_value_t = 4000)]
    max_chars: usize,
}

#[derive(Args)]
struct PlanIngestArgs {
    /// Raw source name, e.g. `feishu/Doc Title.md` or `raw/feishu/Doc Title.md`.
    raw: String,
    /// Maximum number of related pages to return.
    #[arg(long, default_value_t = 12)]
    limit: usize,
}

#[derive(Args)]
struct QueryContextArgs {
    /// User question or search query.
    query: String,
    /// Maximum number of search hits to retrieve.
    #[arg(long, default_value_t = 10)]
    limit: usize,
    /// Maximum number of wiki pages to include with body excerpts.
    #[arg(long, default_value_t = 5)]
    max_pages: usize,
    /// Maximum body characters per included page.
    #[arg(long, default_value_t = 2000)]
    max_body_chars: usize,
}

#[derive(Args)]
struct ReviewArgs {
    /// Number of recent log entries to include.
    #[arg(long, default_value_t = 5)]
    recent_log: usize,
    /// Include a bounded `git diff` excerpt.
    #[arg(long)]
    diff: bool,
    /// Maximum characters of diff to include when --diff is set.
    #[arg(long, default_value_t = 4000)]
    max_diff_chars: usize,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run(cli: &Cli) -> anyhow::Result<()> {
    match &cli.command {
        Command::Init { path } => {
            let root = path.clone().unwrap_or_else(|| cli.workspace.clone());
            let out = ops::init(&root)?;
            if cli.json {
                print_json(&out)?;
            } else {
                println!("Initialized wiki workspace at {}", out.root);
                println!(
                    "  git: {}",
                    if out.git_initialized {
                        "initialized"
                    } else {
                        "skipped (git unavailable)"
                    }
                );
                for c in &out.created {
                    println!("  + {c}");
                }
                println!("\nNext: read AGENTS.md, then `wiki add-raw <file>` to ingest a source.");
            }
        }
        Command::Search(a) => {
            let ws = open_ws(cli)?;
            let hits = ops::search(&ws, &a.query, a.filter.as_deref(), a.limit)?;
            if cli.json {
                print_json(&hits)?;
            } else if hits.is_empty() {
                println!("No results for {:?}.", a.query);
            } else {
                for (i, h) in hits.iter().enumerate() {
                    println!(
                        "{}. [{}] {} ({})  score={:.2}",
                        i + 1,
                        h.category,
                        h.title,
                        h.path,
                        h.score
                    );
                    if !h.snippet.is_empty() {
                        println!("   {}", h.snippet);
                    }
                }
            }
        }
        Command::Read { page } => {
            let ws = open_ws(cli)?;
            let view = ops::read(&ws, page)?;
            if cli.json {
                print_json(&view)?;
            } else {
                println!("# {} ({})", view.frontmatter.title, view.path);
                println!(
                    "type={} status={} tags={:?} sources={:?}",
                    view.frontmatter.kind,
                    view.frontmatter.status,
                    view.frontmatter.tags,
                    view.frontmatter.sources
                );
                if !view.frontmatter.origin.is_empty() {
                    println!("origin: {}", view.frontmatter.origin);
                }
                println!("outbound: {:?}", view.outbound_links);
                println!("backlinks: {:?}", view.backlinks);
                println!("\n{}", view.body);
            }
        }
        Command::Write(a) => {
            let ws = open_ws(cli)?;
            let body = match &a.body {
                Some(b) => b.clone(),
                None => read_stdin()?,
            };
            let out = ops::write_page(
                &ws,
                ops::PageInput {
                    path: a.page.clone(),
                    kind: a.kind.clone(),
                    title: a.title.clone(),
                    tags: a.tag.clone(),
                    sources: a.source.clone(),
                    origin: a.origin.clone(),
                    status: a.status.clone(),
                    body,
                },
            )?;
            if cli.json {
                print_json(&out)?;
            } else {
                println!(
                    "{} {}",
                    if out.created { "Created" } else { "Updated" },
                    out.path
                );
            }
        }
        Command::Edit(a) => {
            let ws = open_ws(cli)?;
            let content = match &a.content {
                Some(c) => c.clone(),
                None => read_stdin()?,
            };
            let mode = if a.replace {
                EditMode::Replace
            } else {
                EditMode::Append
            };
            let out = ops::edit_page(&ws, &a.page, &a.heading, &content, mode)?;
            if cli.json {
                print_json(&out)?;
            } else {
                println!("Edited {} under '{}'", out.path, a.heading);
            }
        }
        Command::Backlinks { page } => {
            let ws = open_ws(cli)?;
            let links = ops::backlinks(&ws, page)?;
            if cli.json {
                print_json(&links)?;
            } else if links.is_empty() {
                println!("No backlinks.");
            } else {
                for l in &links {
                    println!("{l}");
                }
            }
        }
        Command::Index => {
            let ws = open_ws(cli)?;
            let out = ops::reindex(&ws)?;
            if cli.json {
                print_json(&out)?;
            } else {
                println!(
                    "Rebuilt index.md ({} pages) and search index ({} documents).",
                    out.pages_catalogued, out.documents_indexed
                );
            }
        }
        Command::Log(a) => {
            let ws = open_ws(cli)?;
            match (&a.op, &a.title) {
                (Some(op), title) => {
                    let title = title.clone().unwrap_or_default();
                    let entry = ops::log_add(&ws, op, &title, a.detail.as_deref())?;
                    if cli.json {
                        print_json(&entry)?;
                    } else {
                        println!("Logged: {}", entry.header);
                    }
                }
                _ => {
                    let entries = ops::log_tail(&ws, a.tail)?;
                    if cli.json {
                        print_json(&entries)?;
                    } else {
                        for e in &entries {
                            println!("{}", e.header);
                        }
                    }
                }
            }
        }
        Command::Lint => {
            let ws = open_ws(cli)?;
            let report = ops::lint_wiki(&ws)?;
            if cli.json {
                print_json(&report)?;
            } else if report.is_clean() {
                println!("Wiki is clean — no issues found.");
            } else {
                println!("{} issue(s):", report.issues.len());
                for issue in &report.issues {
                    println!(
                        "  [{}] {}: {}",
                        issue.kind.as_str(),
                        issue.subject,
                        issue.message
                    );
                }
            }
        }
        Command::AddRaw(a) => {
            let ws = open_ws(cli)?;
            let out = ops::raw_add_from_file(&ws, &a.file, a.name.as_deref(), a.overwrite)?;
            if cli.json {
                print_json(&out)?;
            } else {
                println!("Registered raw source: {}", out.path);
            }
        }
        Command::ReadRaw(a) => {
            let ws = open_ws(cli)?;
            let chunks = ops::raw_read(&ws, &a.name, a.max_chars)?;
            if cli.json {
                print_json(&chunks)?;
            } else {
                for c in &chunks {
                    let heading = c.heading.as_deref().unwrap_or("(no heading)");
                    println!("--- chunk {}/{} [{}] ---", c.index + 1, c.total, heading);
                    println!("{}", c.text.trim_end());
                }
            }
        }
        Command::PlanIngest(a) => {
            let ws = open_ws(cli)?;
            let plan = ops::plan_ingest(&ws, &a.raw, a.limit)?;
            if cli.json {
                print_json(&plan)?;
            } else {
                println!("Raw: {}", plan.raw.path);
                println!("Title: {}", plan.raw.title);
                println!("Suggested source page: {}", plan.source_page.page);
                if !plan.raw.headings.is_empty() {
                    println!("\nHeadings:");
                    for h in &plan.raw.headings {
                        println!("  - {h}");
                    }
                }
                if !plan.related_pages.is_empty() {
                    println!("\nLikely affected pages:");
                    for p in &plan.related_pages {
                        println!(
                            "  - [{}] {} ({}) via {:?}",
                            p.category, p.title, p.path, p.matched_query
                        );
                    }
                }
                if !plan.warnings.is_empty() {
                    println!("\nWarnings:");
                    for w in &plan.warnings {
                        println!("  - {w}");
                    }
                }
                println!("\nNext steps:");
                for step in &plan.next_steps {
                    println!("  - {step}");
                }
            }
        }
        Command::QueryContext(a) => {
            let ws = open_ws(cli)?;
            let packet = ops::query_context(&ws, &a.query, a.limit, a.max_pages, a.max_body_chars)?;
            if cli.json {
                print_json(&packet)?;
            } else {
                println!("Query: {}", packet.query);
                println!(
                    "{} hit(s), {} page excerpt(s), {} raw hit(s).",
                    packet.hits.len(),
                    packet.pages.len(),
                    packet.raw_hits.len()
                );
                for page in &packet.pages {
                    println!(
                        "\n# {} ({}) score={:.2}",
                        page.title, page.path, page.search_score
                    );
                    if !page.search_snippet.is_empty() {
                        println!("{}", page.search_snippet);
                    }
                    println!("backlinks: {:?}", page.backlinks);
                }
                println!("\nPersistence steps:");
                for step in &packet.persistence_steps {
                    println!("  - {step}");
                }
            }
        }
        Command::Review(a) => {
            let ws = open_ws(cli)?;
            let review = ops::review(&ws, a.recent_log, a.diff, a.max_diff_chars)?;
            if cli.json {
                print_json(&review)?;
            } else {
                println!("Lint issues: {}", review.lint.issues.len());
                println!(
                    "Git: {}{}",
                    if review.git.available {
                        "available"
                    } else {
                        "unavailable"
                    },
                    if review.git.dirty {
                        ", dirty"
                    } else {
                        ", clean"
                    }
                );
                for f in &review.git.changed_files {
                    println!("  {} {}", f.status, f.path);
                }
                if !review.recent_log.is_empty() {
                    println!("\nRecent log:");
                    for entry in &review.recent_log {
                        println!("  {}", entry.header);
                    }
                }
                println!("\nRecommended actions:");
                for action in &review.recommended_actions {
                    println!("  - {action}");
                }
            }
        }
    }
    Ok(())
}

/// Open the workspace given by `--workspace`.
fn open_ws(cli: &Cli) -> anyhow::Result<Workspace> {
    Ok(Workspace::open(&cli.workspace)?)
}

/// Read all of stdin to a string.
fn read_stdin() -> anyhow::Result<String> {
    let mut buf = String::new();
    std::io::stdin().read_to_string(&mut buf)?;
    Ok(buf)
}

/// Pretty-print any serializable value as JSON.
fn print_json<T: serde::Serialize>(value: &T) -> anyhow::Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}
