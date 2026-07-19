//! High-level operations that compose the primitives (workspace, search, index,
//! log) into the units both front-ends expose as tools/commands.
//!
//! Keeping this orchestration in `wiki-core` — rather than duplicating it in the
//! CLI and the MCP server — is what guarantees the two front-ends behave
//! identically. The CLI and MCP layers only parse their own inputs, call these
//! functions, and format the returned (serializable) result structs.

use std::collections::BTreeSet;
use std::process::Command;

use serde::Serialize;

use crate::error::{Result, WikiError};
use crate::index::rebuild_index;
use crate::lint::{lint, LintReport};
use crate::logbook::{log_append, log_recent, LogEntry};
use crate::page::{Frontmatter, Page, PageView};
use crate::search::{SearchHit, SearchIndex};
use crate::workspace::{today, EditMode, Workspace};

/// Input for creating/overwriting a page.
#[derive(Debug, Clone, Default)]
pub struct PageInput {
    pub path: String,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub tags: Vec<String>,
    pub sources: Vec<String>,
    pub origin: Option<String>,
    pub status: Option<String>,
    pub body: String,
}

/// Result of [`init`].
#[derive(Debug, Clone, Serialize)]
pub struct InitOutcome {
    pub root: String,
    pub git_initialized: bool,
    pub created: Vec<String>,
}

/// Result of a write/edit.
#[derive(Debug, Clone, Serialize)]
pub struct WriteOutcome {
    pub path: String,
    /// Whether the page was newly created (vs. overwritten/edited).
    pub created: bool,
    /// Whether the search index was updated.
    pub reindexed: bool,
}

/// Result of a full reindex (`wiki_index`).
#[derive(Debug, Clone, Serialize)]
pub struct IndexOutcome {
    /// Pages catalogued into `index.md`.
    pub pages_catalogued: usize,
    /// Documents written to the search index (pages + raw sources).
    pub documents_indexed: usize,
}

/// Result of registering a raw source.
#[derive(Debug, Clone, Serialize)]
pub struct RawAddOutcome {
    pub path: String,
}

/// A page that is likely to be affected by a new source.
#[derive(Debug, Clone, Serialize)]
pub struct RelatedPage {
    pub path: String,
    pub category: String,
    pub title: String,
    pub score: f32,
    pub matched_query: String,
    pub reason: String,
    pub snippet: String,
}

/// Lightweight outline of a raw markdown source.
#[derive(Debug, Clone, Serialize)]
pub struct RawOutline {
    pub path: String,
    pub title: String,
    pub headings: Vec<String>,
    pub char_count: usize,
}

/// Candidate destination for the source summary page.
#[derive(Debug, Clone, Serialize)]
pub struct SourcePagePlan {
    pub page: String,
    pub title: String,
    pub sources: Vec<String>,
    pub body_template: String,
}

/// Result of planning an ingest before the agent starts writing pages.
#[derive(Debug, Clone, Serialize)]
pub struct IngestPlanOutcome {
    pub raw: RawOutline,
    pub source_page: SourcePagePlan,
    pub related_pages: Vec<RelatedPage>,
    pub semantic_checks: Vec<String>,
    pub next_steps: Vec<String>,
    pub warnings: Vec<String>,
}

/// Bounded page evidence returned for a query.
#[derive(Debug, Clone, Serialize)]
pub struct PageEvidence {
    pub path: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub status: String,
    pub sources: Vec<String>,
    pub origin: String,
    pub search_score: f32,
    pub search_snippet: String,
    pub body_excerpt: String,
    pub outbound_links: Vec<String>,
    pub backlinks: Vec<String>,
}

/// Evidence packet for answering a query without blindly reading the whole wiki.
#[derive(Debug, Clone, Serialize)]
pub struct QueryContextOutcome {
    pub query: String,
    pub hits: Vec<SearchHit>,
    pub pages: Vec<PageEvidence>,
    pub raw_hits: Vec<SearchHit>,
    pub semantic_checks: Vec<String>,
    pub persistence_steps: Vec<String>,
}

/// A file changed according to git status.
#[derive(Debug, Clone, Serialize)]
pub struct GitChangedFile {
    pub status: String,
    pub path: String,
}

/// Git-oriented review context for a wiki maintenance pass.
#[derive(Debug, Clone, Serialize)]
pub struct GitReview {
    pub available: bool,
    pub dirty: bool,
    pub changed_files: Vec<GitChangedFile>,
    pub diff_excerpt: String,
}

/// Combined maintenance packet for the agent before it commits or asks for review.
#[derive(Debug, Clone, Serialize)]
pub struct ReviewOutcome {
    pub lint: LintReport,
    pub recent_log: Vec<LogEntry>,
    pub git: GitReview,
    pub semantic_checks: Vec<String>,
    pub recommended_actions: Vec<String>,
}

/// Scaffold a new workspace.
pub fn init(root: &str) -> Result<InitOutcome> {
    let (ws, git) = Workspace::init(root)?;
    Ok(InitOutcome {
        root: ws.root().to_string_lossy().to_string(),
        git_initialized: git,
        created: vec![
            "AGENTS.md".into(),
            "raw/".into(),
            "wiki/overview.md".into(),
            "wiki/index.md".into(),
            "wiki/log.md".into(),
            "wiki/entities/".into(),
            "wiki/concepts/".into(),
            "wiki/sources/".into(),
            ".gitignore".into(),
        ],
    })
}

/// Search the wiki, lazily building the index on first use.
pub fn search(
    ws: &Workspace,
    query: &str,
    filter: Option<&str>,
    limit: usize,
) -> Result<Vec<SearchHit>> {
    let si = SearchIndex::open(ws)?;
    // Lazily populate on first search so a freshly-initialized workspace works
    // without an explicit `wiki index` first.
    if si.num_docs()? == 0 {
        SearchIndex::reindex_all(ws)?;
        let si = SearchIndex::open(ws)?;
        return si.search(query, filter, limit.max(1));
    }
    si.search(query, filter, limit.max(1))
}

/// Read a page (body, frontmatter, outbound links, backlinks).
pub fn read(ws: &Workspace, page: &str) -> Result<PageView> {
    ws.read_page(page)
}

/// List the pages that link to `page`.
pub fn backlinks(ws: &Workspace, page: &str) -> Result<Vec<String>> {
    let (path, _) = ws.load_page(page)?;
    let graph = ws.build_link_graph()?;
    Ok(graph.backlinks(&path).to_vec())
}

/// Create or overwrite a page, then incrementally update the search index.
///
/// Frontmatter dates are managed here: `created` is preserved from an existing
/// page (or set to today for a new one); `updated` is always set to today.
pub fn write_page(ws: &Workspace, input: PageInput) -> Result<WriteOutcome> {
    let canon = ws.canonical_page_path(&input.path);

    // Preserve `created` and any extra frontmatter from an existing page.
    let (existed, mut fm) = match ws.load_page(&canon) {
        Ok((_, existing)) => (true, existing.frontmatter),
        Err(_) => (false, Frontmatter::default()),
    };

    if let Some(kind) = input.kind {
        fm.kind = kind;
    } else if !existed || fm.kind == "note" {
        fm.kind = infer_kind(&canon);
    }
    if let Some(title) = input.title {
        fm.title = title;
    }
    if !input.tags.is_empty() {
        fm.tags = input.tags;
    }
    if !input.sources.is_empty() {
        fm.sources = input.sources;
    }
    if let Some(origin) = input.origin {
        fm.origin = origin;
    }
    if let Some(status) = input.status {
        fm.status = status;
    }
    if fm.created.is_empty() {
        fm.created = today();
    }
    fm.updated = today();

    let page = Page::new(fm, input.body);
    let path = ws.write_page(&canon, &page)?;

    let reindexed = SearchIndex::open(ws)
        .and_then(|si| si.upsert_page(ws, &path))
        .is_ok();

    Ok(WriteOutcome {
        path,
        created: !existed,
        reindexed,
    })
}

/// Patch a page under a heading (append or replace), then re-index it.
pub fn edit_page(
    ws: &Workspace,
    page: &str,
    heading: &str,
    content: &str,
    mode: EditMode,
) -> Result<WriteOutcome> {
    let path = ws.edit_page(page, heading, content, mode)?;
    let reindexed = SearchIndex::open(ws)
        .and_then(|si| si.upsert_page(ws, &path))
        .is_ok();
    Ok(WriteOutcome {
        path,
        created: false,
        reindexed,
    })
}

/// Rebuild `index.md` and the full search index.
pub fn reindex(ws: &Workspace) -> Result<IndexOutcome> {
    let pages = rebuild_index(ws)?;
    let docs = SearchIndex::reindex_all(ws)?;
    Ok(IndexOutcome {
        pages_catalogued: pages,
        documents_indexed: docs,
    })
}

/// Append a log entry.
pub fn log_add(ws: &Workspace, op: &str, title: &str, detail: Option<&str>) -> Result<LogEntry> {
    log_append(ws, op, title, detail, None)
}

/// Read the most recent `n` log entries.
pub fn log_tail(ws: &Workspace, n: usize) -> Result<Vec<LogEntry>> {
    log_recent(ws, n)
}

/// Run the structural lint.
pub fn lint_wiki(ws: &Workspace) -> Result<LintReport> {
    lint(ws)
}

/// Register a raw source, then incrementally index it.
pub fn raw_add(
    ws: &Workspace,
    name: &str,
    content: &str,
    overwrite: bool,
) -> Result<RawAddOutcome> {
    let rel = crate::ingest::raw_add(ws, name, content, overwrite)?;
    // Best-effort incremental index of the new source.
    let _ = SearchIndex::open(ws).and_then(|si| si.upsert_raw(ws, &rel));
    Ok(RawAddOutcome { path: rel })
}

/// Read a raw source in chunks.
pub fn raw_read(ws: &Workspace, name: &str, max_chars: usize) -> Result<Vec<crate::ingest::Chunk>> {
    crate::ingest::raw_read(ws, name, max_chars)
}

/// Read a raw source from an absolute/relative filesystem path and register it
/// under `raw/` (used by the CLI's `add-raw <file>` convenience).
pub fn raw_add_from_file(
    ws: &Workspace,
    file: &str,
    dest_name: Option<&str>,
    overwrite: bool,
) -> Result<RawAddOutcome> {
    let content = std::fs::read_to_string(file)
        .map_err(|e| WikiError::io(std::path::PathBuf::from(file), e))?;
    let name = dest_name
        .map(|s| s.to_string())
        .or_else(|| {
            std::path::Path::new(file)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
        })
        .ok_or_else(|| WikiError::Other(format!("cannot derive raw name from {file}")))?;
    raw_add(ws, &name, &content, overwrite)
}

/// Plan a source ingest: identify a summary page name, candidate impacted pages,
/// and semantic checks the agent should perform before writing.
pub fn plan_ingest(ws: &Workspace, raw_name: &str, limit: usize) -> Result<IngestPlanOutcome> {
    let raw_rel = normalize_raw_ref(raw_name);
    let text = ws.read_file(&raw_rel)?;
    let title = source_title(&text, &raw_rel);
    let headings = markdown_headings(&text, 20);
    let source_stem = title_to_page_stem(&title, &raw_title_from_path(&raw_rel));
    let source_page = SourcePagePlan {
        page: format!("sources/{source_stem}"),
        title: title.clone(),
        sources: vec![raw_rel.clone()],
        body_template: format!(
            "# {title}\n\n## Summary\n\n<!-- Agent: summarize the durable facts from {raw_rel}. -->\n\n## Key Links\n\n<!-- Agent: add [[entities/...]] and [[concepts/...]] links, then update those pages. -->\n"
        ),
    };

    let queries = ingest_queries(&title, &headings);
    let mut related_pages = related_pages(ws, &queries, limit.max(1) * 2)?;
    related_pages.retain(|p| p.path != format!("wiki/{}.md", source_page.page));
    related_pages.truncate(limit.max(1));

    let mut warnings = Vec::new();
    if headings.is_empty() {
        warnings
            .push("raw source has no markdown headings; agent should read chunks carefully".into());
    }
    if related_pages.is_empty() {
        warnings.push("no related wiki pages found; likely create new entity/concept pages".into());
    }

    Ok(IngestPlanOutcome {
        raw: RawOutline {
            path: raw_rel.clone(),
            title,
            headings,
            char_count: text.chars().count(),
        },
        source_page,
        related_pages,
        semantic_checks: ingest_semantic_checks(),
        next_steps: vec![
            "raw_read the source in chunks; do not rely only on this outline".into(),
            "write a sources/ summary page with sources/origin provenance".into(),
            "update every affected entities/ and concepts/ page with bidirectional [[links]]"
                .into(),
            "flag contradictions inline and set status=needs-review when unsure".into(),
            "run wiki_index, wiki_lint, and wiki_log when the ingest is complete".into(),
        ],
        warnings,
    })
}

/// Build a bounded evidence packet for a query.
pub fn query_context(
    ws: &Workspace,
    query: &str,
    limit: usize,
    max_pages: usize,
    max_body_chars: usize,
) -> Result<QueryContextOutcome> {
    let hits = search(ws, query, None, limit.max(1))?;
    let mut pages = Vec::new();
    let mut raw_hits = Vec::new();

    for hit in &hits {
        if hit.scope == "raw" || hit.category == "raw" {
            raw_hits.push(hit.clone());
            continue;
        }
        if is_generated_meta_page(&hit.path) {
            continue;
        }
        if pages.len() >= max_pages.max(1) {
            continue;
        }
        if let Ok(view) = read(ws, &hit.path) {
            pages.push(PageEvidence {
                path: view.path,
                kind: view.frontmatter.kind,
                title: view.frontmatter.title,
                status: view.frontmatter.status,
                sources: view.frontmatter.sources,
                origin: view.frontmatter.origin,
                search_score: hit.score,
                search_snippet: hit.snippet.clone(),
                body_excerpt: truncate_chars(&view.body, max_body_chars.max(400)),
                outbound_links: view.outbound_links,
                backlinks: view.backlinks,
            });
        }
    }

    Ok(QueryContextOutcome {
        query: query.to_string(),
        hits,
        pages,
        raw_hits,
        semantic_checks: vec![
            "Verify whether page summaries agree with their cited source pages/raw files.".into(),
            "Follow outbound links/backlinks when the answer depends on relationships or history.".into(),
            "Call out uncertainty when only raw hits exist and no curated wiki page supports the claim.".into(),
        ],
        persistence_steps: vec![
            "Answer with citations to wiki pages and/or raw sources.".into(),
            "If the synthesis is durable, write it back as a page or update an existing page.".into(),
            "Log durable query work with wiki_log op=query.".into(),
        ],
    })
}

/// Return lint, recent log entries, and git diff context for a review pass.
pub fn review(
    ws: &Workspace,
    recent_log: usize,
    include_diff: bool,
    max_diff_chars: usize,
) -> Result<ReviewOutcome> {
    let lint_report = lint_wiki(ws)?;
    let log = log_tail(ws, recent_log)?;
    let git = git_review(ws, include_diff, max_diff_chars.max(400));

    let mut recommended_actions = Vec::new();
    if !lint_report.is_clean() {
        recommended_actions
            .push("Fix or explicitly defer every wiki_lint issue before commit.".into());
    }
    if git.dirty {
        recommended_actions
            .push("Review changed files, explain why each wiki page changed, then commit.".into());
    } else {
        recommended_actions
            .push("No git changes detected; run ingest/query before committing.".into());
    }
    recommended_actions.push(
        "If the changes came from a query, ensure the useful synthesis was persisted and logged."
            .into(),
    );

    Ok(ReviewOutcome {
        lint: lint_report,
        recent_log: log,
        git,
        semantic_checks: vec![
            "Look for contradictions introduced by newer sources, not just dead links.".into(),
            "Check whether overview.md/index.md still represent the current synthesis.".into(),
            "Check whether repeated important terms deserve new concepts/ pages.".into(),
            "Check whether changed pages have reciprocal links where a reader would expect them."
                .into(),
        ],
        recommended_actions,
    })
}

/// Infer a default page `type` from its folder.
fn infer_kind(canon: &str) -> String {
    if canon.contains("/entities/") {
        "entity".into()
    } else if canon.contains("/concepts/") {
        "concept".into()
    } else if canon.contains("/sources/") {
        "source".into()
    } else {
        "note".into()
    }
}

fn normalize_raw_ref(name: &str) -> String {
    let clean = name.trim().trim_start_matches("./").replace('\\', "/");
    if clean.starts_with("raw/") {
        clean
    } else {
        format!("raw/{clean}")
    }
}

fn raw_title_from_path(raw_rel: &str) -> String {
    raw_rel
        .rsplit('/')
        .next()
        .unwrap_or(raw_rel)
        .trim_end_matches(".md")
        .to_string()
}

fn source_title(text: &str, raw_rel: &str) -> String {
    title_comment(text)
        .or_else(|| html_title(text))
        .or_else(|| first_markdown_heading(text))
        .unwrap_or_else(|| raw_title_from_path(raw_rel))
}

fn title_comment(text: &str) -> Option<String> {
    for line in text.lines().take(40) {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("<!-- title:") {
            let title = rest.trim_end_matches("-->").trim();
            if !title.is_empty() {
                return Some(title.to_string());
            }
        }
    }
    None
}

fn html_title(text: &str) -> Option<String> {
    for line in text.lines().take(40) {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("<title>") {
            if let Some(title) = rest.strip_suffix("</title>") {
                let title = title.trim();
                if !title.is_empty() {
                    return Some(title.to_string());
                }
            }
        }
    }
    None
}

fn first_markdown_heading(text: &str) -> Option<String> {
    markdown_headings(text, 1).into_iter().next()
}

fn markdown_headings(text: &str, limit: usize) -> Vec<String> {
    let mut out = Vec::new();
    for line in text.lines() {
        let t = line.trim_start();
        let hashes = t.chars().take_while(|&c| c == '#').count();
        if (1..=6).contains(&hashes) && t[hashes..].starts_with(' ') {
            let heading = t[hashes..].trim();
            if !heading.is_empty() {
                out.push(heading.to_string());
                if out.len() >= limit {
                    break;
                }
            }
        }
    }
    out
}

fn title_to_page_stem(title: &str, fallback: &str) -> String {
    let mut stem = title
        .trim()
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\0' => '-',
            _ => c,
        })
        .collect::<String>();
    stem = stem
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(['-', '.', ' '])
        .to_string();
    if stem.is_empty() {
        fallback.to_string()
    } else {
        stem
    }
}

fn ingest_queries(title: &str, headings: &[String]) -> Vec<String> {
    let mut queries = Vec::new();
    push_query(&mut queries, title);
    for heading in headings.iter().take(8) {
        push_query(&mut queries, heading);
    }
    for token in ascii_keywords(title).into_iter().take(6) {
        push_query(&mut queries, &token);
    }
    queries
}

fn push_query(out: &mut Vec<String>, q: &str) {
    let q = q.trim();
    if q.chars().count() >= 2 && q.chars().count() <= 80 && !out.iter().any(|x| x == q) {
        out.push(q.to_string());
    }
}

fn ascii_keywords(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "the", "and", "for", "with", "from", "this", "that", "into", "about", "wiki", "doc",
        "docs", "design", "document",
    ];
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for word in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .map(|w| w.to_ascii_lowercase())
    {
        if word.len() < 3 || STOP.contains(&word.as_str()) || !seen.insert(word.clone()) {
            continue;
        }
        out.push(word);
    }
    out
}

fn related_pages(ws: &Workspace, queries: &[String], cap: usize) -> Result<Vec<RelatedPage>> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::new();
    for query in queries {
        for hit in search(ws, query, Some("wiki"), cap.max(1))? {
            if hit.category == "raw" || is_generated_meta_page(&hit.path) {
                continue;
            }
            if !seen.insert(hit.path.clone()) {
                continue;
            }
            out.push(RelatedPage {
                path: hit.path,
                category: hit.category,
                title: hit.title,
                score: hit.score,
                matched_query: query.clone(),
                reason: "matched source title/heading during ingest planning".into(),
                snippet: hit.snippet,
            });
            if out.len() >= cap {
                return Ok(out);
            }
        }
    }
    Ok(out)
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push_str("\n\n<!-- truncated by agent-wiki -->");
    out
}

fn is_generated_meta_page(path: &str) -> bool {
    matches!(path, "wiki/index.md" | "wiki/log.md")
}

fn ingest_semantic_checks() -> Vec<String> {
    vec![
        "Identify entities, concepts, decisions, APIs, projects, and recurring terms worth their own pages.".into(),
        "Compare new claims with existing related pages; mark contradictions inline instead of silently overwriting.".into(),
        "Update overview.md when the source changes the top-level thesis or project map.".into(),
        "Add reciprocal context: pages linked from the source summary should mention the source where relevant.".into(),
        "Preserve provenance with sources: raw/... and origin: when available.".into(),
    ]
}

fn git_review(ws: &Workspace, include_diff: bool, max_diff_chars: usize) -> GitReview {
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(ws.root())
        .output();
    let Ok(status) = status else {
        return GitReview {
            available: false,
            dirty: false,
            changed_files: Vec::new(),
            diff_excerpt: String::new(),
        };
    };
    if !status.status.success() {
        return GitReview {
            available: false,
            dirty: false,
            changed_files: Vec::new(),
            diff_excerpt: String::from_utf8_lossy(&status.stderr).trim().to_string(),
        };
    }

    let stdout = String::from_utf8_lossy(&status.stdout);
    let changed_files = stdout
        .lines()
        .filter_map(parse_git_status_line)
        .collect::<Vec<_>>();
    let dirty = !changed_files.is_empty();
    let diff_excerpt = if include_diff && dirty {
        Command::new("git")
            .args(["diff", "--"])
            .current_dir(ws.root())
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| truncate_chars(&String::from_utf8_lossy(&o.stdout), max_diff_chars))
            .unwrap_or_default()
    } else {
        String::new()
    };

    GitReview {
        available: true,
        dirty,
        changed_files,
        diff_excerpt,
    }
}

fn parse_git_status_line(line: &str) -> Option<GitChangedFile> {
    if line.len() < 4 {
        return None;
    }
    let status = line[..2].trim().to_string();
    let path = line[3..]
        .split(" -> ")
        .last()
        .unwrap_or(&line[3..])
        .to_string();
    Some(GitChangedFile { status, path })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let (ws, _) = Workspace::init(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn write_infers_kind_and_dates() {
        let (_d, ws) = init_ws();
        let out = write_page(
            &ws,
            PageInput {
                path: "entities/ada".into(),
                title: Some("Ada".into()),
                body: "Ada text.\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(out.created);
        let (_, page) = ws.load_page("entities/ada").unwrap();
        assert_eq!(page.frontmatter.kind, "entity");
        assert!(!page.frontmatter.created.is_empty());
        assert!(!page.frontmatter.updated.is_empty());
    }

    #[test]
    fn overwrite_preserves_created() {
        let (_d, ws) = init_ws();
        write_page(
            &ws,
            PageInput {
                path: "concepts/x".into(),
                title: Some("X".into()),
                body: "v1".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, p1) = ws.load_page("concepts/x").unwrap();
        let created = p1.frontmatter.created.clone();

        let out = write_page(
            &ws,
            PageInput {
                path: "concepts/x".into(),
                body: "v2".into(),
                ..Default::default()
            },
        )
        .unwrap();
        assert!(!out.created);
        let (_, p2) = ws.load_page("concepts/x").unwrap();
        assert_eq!(p2.frontmatter.created, created);
        assert_eq!(p2.body.trim(), "v2");
    }

    #[test]
    fn write_sets_and_preserves_origin() {
        let (_d, ws) = init_ws();
        // First write records an origin URL.
        write_page(
            &ws,
            PageInput {
                path: "sources/feishu-doc".into(),
                title: Some("Feishu Doc".into()),
                origin: Some("https://x.feishu.cn/docx/abc".into()),
                body: "summary".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, p1) = ws.load_page("sources/feishu-doc").unwrap();
        assert_eq!(p1.frontmatter.origin, "https://x.feishu.cn/docx/abc");

        // A subsequent write that does not provide `origin` preserves it.
        write_page(
            &ws,
            PageInput {
                path: "sources/feishu-doc".into(),
                body: "updated summary".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, p2) = ws.load_page("sources/feishu-doc").unwrap();
        assert_eq!(p2.frontmatter.origin, "https://x.feishu.cn/docx/abc");

        // Providing a new `origin` overwrites the old one.
        write_page(
            &ws,
            PageInput {
                path: "sources/feishu-doc".into(),
                origin: Some("https://x.feishu.cn/docx/def".into()),
                body: "re-sourced".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let (_, p3) = ws.load_page("sources/feishu-doc").unwrap();
        assert_eq!(p3.frontmatter.origin, "https://x.feishu.cn/docx/def");
    }

    #[test]
    fn search_lazily_builds_index() {
        let (_d, ws) = init_ws();
        write_page(
            &ws,
            PageInput {
                path: "entities/ada".into(),
                title: Some("Ada".into()),
                body: "Ada the mathematician.\n".into(),
                ..Default::default()
            },
        )
        .unwrap();
        // Force-remove the index so search must rebuild it.
        let _ = std::fs::remove_dir_all(ws.index_dir());
        let hits = search(&ws, "mathematician", None, 10).unwrap();
        assert!(hits.iter().any(|h| h.path == "wiki/entities/ada.md"));
    }

    #[test]
    fn full_pipeline_index_and_log() {
        let (_d, ws) = init_ws();
        write_page(
            &ws,
            PageInput {
                path: "concepts/x".into(),
                title: Some("X".into()),
                body: "content".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let idx = reindex(&ws).unwrap();
        assert!(idx.pages_catalogued >= 1);
        assert!(idx.documents_indexed >= 1);

        log_add(&ws, "ingest", "Test", Some("detail")).unwrap();
        let recent = log_tail(&ws, 5).unwrap();
        assert_eq!(recent.last().unwrap().op, "ingest");
    }

    #[test]
    fn plan_ingest_suggests_source_page_and_related_pages() {
        let (_d, ws) = init_ws();
        raw_add(
            &ws,
            "ada.md",
            "# Ada\n\nA note about Ada and engines.\n",
            false,
        )
        .unwrap();
        write_page(
            &ws,
            PageInput {
                path: "entities/ada".into(),
                title: Some("Ada".into()),
                body: "Ada is already tracked here.\n".into(),
                ..Default::default()
            },
        )
        .unwrap();

        let plan = plan_ingest(&ws, "ada.md", 5).unwrap();
        assert_eq!(plan.raw.title, "Ada");
        assert_eq!(plan.source_page.page, "sources/Ada");
        assert!(plan
            .related_pages
            .iter()
            .any(|p| p.path == "wiki/entities/ada.md"));
        assert!(!plan.semantic_checks.is_empty());
    }

    #[test]
    fn plan_ingest_prefers_exported_title_over_first_heading() {
        let (_d, ws) = init_ws();
        raw_add(
            &ws,
            "feishu/jakarta.md",
            "<!-- title: 雅加达计费项验收 -->\n\n<title>雅加达计费项验收</title>\n\n# 结论\n\n符合预期。\n",
            false,
        )
        .unwrap();

        let plan = plan_ingest(&ws, "feishu/jakarta.md", 3).unwrap();
        assert_eq!(plan.raw.title, "雅加达计费项验收");
        assert_eq!(plan.source_page.page, "sources/雅加达计费项验收");
        assert_eq!(plan.raw.headings[0], "结论");
    }

    #[test]
    fn query_context_returns_bounded_page_evidence() {
        let (_d, ws) = init_ws();
        write_page(
            &ws,
            PageInput {
                path: "concepts/alpha".into(),
                title: Some("Alpha".into()),
                body: format!("alpha keyword. {}", "x".repeat(700)),
                ..Default::default()
            },
        )
        .unwrap();

        let packet = query_context(&ws, "alpha", 10, 2, 400).unwrap();
        assert_eq!(packet.query, "alpha");
        assert_eq!(packet.pages.len(), 1);
        assert_eq!(packet.pages[0].path, "wiki/concepts/alpha.md");
        assert!(packet.pages[0]
            .body_excerpt
            .contains("truncated by agent-wiki"));
        assert!(!packet.persistence_steps.is_empty());
    }

    #[test]
    fn review_includes_lint_log_and_actions() {
        let (_d, ws) = init_ws();
        log_add(&ws, "query", "Alpha", Some("stored synthesis")).unwrap();

        let out = review(&ws, 5, false, 1000).unwrap();
        assert_eq!(out.recent_log.last().unwrap().op, "query");
        assert!(!out.recommended_actions.is_empty());
        assert!(!out.semantic_checks.is_empty());
    }
}
