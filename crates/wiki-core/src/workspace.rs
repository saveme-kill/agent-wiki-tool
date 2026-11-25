//! The wiki workspace: a git repository laid out in three layers (raw sources,
//! the wiki, the schema/`AGENTS.md`), plus deterministic file operations over it.
//!
//! Layout produced by [`Workspace::init`]:
//!
//! ```text
//! workspace/
//!   AGENTS.md           # schema: conventions + Ingest/Query/Lint workflows
//!   raw/                # immutable source documents (agent reads, never edits)
//!   wiki/
//!     overview.md       # top-level synthesis / evolving thesis
//!     index.md          # catalog: link + one-line summary per page, by category
//!     log.md            # append-only chronological record
//!     entities/  concepts/  sources/
//!   .wiki/index/        # tantivy full-text index (gitignored)
//!   .gitignore
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::error::{Result, WikiError};
use crate::link::{extract_wikilinks, LinkGraph};
use crate::page::{FrontmatterView, Page, PageView};

/// How [`Workspace::edit_page`] applies new content to a heading section.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditMode {
    /// Append `content` to the end of the section body.
    Append,
    /// Replace the section body (everything under the heading up to the next
    /// heading of the same or higher level) with `content`.
    Replace,
}

/// A handle to a wiki workspace rooted at a directory.
#[derive(Debug, Clone)]
pub struct Workspace {
    root: PathBuf,
}

impl Workspace {
    /// Open an existing workspace, validating that it looks like one.
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let ws = Workspace { root: root.clone() };
        if !ws.agents_file().exists() || !ws.wiki_dir().exists() {
            return Err(WikiError::NotAWorkspace(root));
        }
        Ok(ws)
    }

    /// Open a workspace without validation (used right after `init`).
    pub fn open_unchecked(root: impl AsRef<Path>) -> Self {
        Workspace {
            root: root.as_ref().to_path_buf(),
        }
    }

    /// Scaffold a new workspace: directory tree, `AGENTS.md`, seed pages,
    /// `.gitignore`, and a git repository (best-effort).
    ///
    /// Returns `(workspace, git_initialized)`.
    pub fn init(root: impl AsRef<Path>) -> Result<(Self, bool)> {
        let root = root.as_ref().to_path_buf();
        let ws = Workspace { root: root.clone() };

        if ws.agents_file().exists() {
            return Err(WikiError::AlreadyInitialized(root));
        }

        // Directory tree.
        for dir in [
            ws.root.clone(),
            ws.raw_dir(),
            ws.wiki_dir(),
            ws.wiki_dir().join("entities"),
            ws.wiki_dir().join("concepts"),
            ws.wiki_dir().join("sources"),
            ws.index_dir(),
        ] {
            std::fs::create_dir_all(&dir).map_err(|e| WikiError::io(&dir, e))?;
        }

        // Keep otherwise-empty dirs in git.
        for keep in [
            ws.raw_dir().join(".gitkeep"),
            ws.wiki_dir().join("entities/.gitkeep"),
            ws.wiki_dir().join("concepts/.gitkeep"),
            ws.wiki_dir().join("sources/.gitkeep"),
        ] {
            ws.write_raw_abs(&keep, "")?;
        }

        let today = today();

        ws.write_raw_abs(&ws.agents_file(), AGENTS_TEMPLATE)?;
        ws.write_raw_abs(&ws.root.join(".gitignore"), GITIGNORE_TEMPLATE)?;
        ws.write_raw_abs(
            &ws.wiki_dir().join("overview.md"),
            &overview_template(&today),
        )?;
        ws.write_raw_abs(&ws.wiki_dir().join("index.md"), &index_template(&today))?;
        ws.write_raw_abs(&ws.wiki_dir().join("log.md"), LOG_TEMPLATE)?;

        let git = ws.git_init();
        Ok((ws, git))
    }

    /// Best-effort `git init` + initial commit. Returns whether git succeeded.
    fn git_init(&self) -> bool {
        if self.root.join(".git").exists() {
            return true;
        }
        let ok = Command::new("git")
            .arg("init")
            .arg("-q")
            .current_dir(&self.root)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !ok {
            return false;
        }
        // Stage and commit the scaffold; ignore failures (e.g. missing git identity).
        let _ = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&self.root)
            .status();
        let _ = Command::new("git")
            .args([
                "-c",
                "user.email=wiki@localhost",
                "-c",
                "user.name=agent-wiki",
                "commit",
                "-q",
                "-m",
                "chore: initialize wiki workspace",
            ])
            .current_dir(&self.root)
            .status();
        true
    }

    // --- path helpers -----------------------------------------------------

    /// Workspace root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn agents_file(&self) -> PathBuf {
        self.root.join("AGENTS.md")
    }

    pub fn raw_dir(&self) -> PathBuf {
        self.root.join("raw")
    }

    pub fn wiki_dir(&self) -> PathBuf {
        self.root.join("wiki")
    }

    /// On-disk tantivy index directory.
    pub fn index_dir(&self) -> PathBuf {
        self.root.join(".wiki").join("index")
    }

    /// Resolve a workspace-relative path to an absolute one, rejecting any
    /// path that escapes the workspace root.
    pub fn abs(&self, rel: &str) -> Result<PathBuf> {
        let rel = rel.trim().trim_start_matches("./").replace('\\', "/");
        if rel.split('/').any(|c| c == "..") {
            return Err(WikiError::InvalidPagePath {
                path: rel,
                reason: "path may not contain '..'".into(),
            });
        }
        Ok(self.root.join(rel))
    }

    /// Turn an absolute path back into a workspace-relative one (forward slashes).
    pub fn rel(&self, abs: &Path) -> Option<String> {
        abs.strip_prefix(&self.root)
            .ok()
            .map(|p| p.to_string_lossy().replace('\\', "/"))
    }

    /// Canonicalize a user-supplied page identifier into a workspace-relative
    /// `wiki/...md` path. Accepts `entities/ada`, `entities/ada.md`,
    /// `wiki/entities/ada.md`, `overview`, etc.
    pub fn canonical_page_path(&self, input: &str) -> String {
        let mut p = input.trim().trim_start_matches("./").replace('\\', "/");
        if !p.ends_with(".md") {
            p.push_str(".md");
        }
        if p.starts_with("wiki/") || p.starts_with("raw/") {
            p
        } else {
            format!("wiki/{p}")
        }
    }

    // --- raw file IO ------------------------------------------------------

    fn read_file_abs(&self, abs: &Path) -> Result<String> {
        std::fs::read_to_string(abs).map_err(|e| WikiError::io(abs, e))
    }

    fn write_raw_abs(&self, abs: &Path, contents: &str) -> Result<()> {
        if let Some(parent) = abs.parent() {
            std::fs::create_dir_all(parent).map_err(|e| WikiError::io(parent, e))?;
        }
        std::fs::write(abs, contents).map_err(|e| WikiError::io(abs, e))
    }

    /// Read a workspace-relative file to a string.
    pub fn read_file(&self, rel: &str) -> Result<String> {
        let abs = self.abs(rel)?;
        self.read_file_abs(&abs)
    }

    // --- page/raw listing -------------------------------------------------

    /// List all markdown pages under `wiki/`, as workspace-relative paths, sorted.
    pub fn list_pages(&self) -> Result<Vec<String>> {
        self.list_md_under(&self.wiki_dir())
    }

    /// List all files under `raw/`, as workspace-relative paths, sorted.
    pub fn list_raw(&self) -> Result<Vec<String>> {
        let raw = self.raw_dir();
        let mut out = Vec::new();
        if !raw.exists() {
            return Ok(out);
        }
        for entry in walkdir::WalkDir::new(&raw)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file() {
                let name = entry.file_name().to_string_lossy();
                if name == ".gitkeep" {
                    continue;
                }
                if let Some(rel) = self.rel(entry.path()) {
                    out.push(rel);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    fn list_md_under(&self, dir: &Path) -> Result<Vec<String>> {
        let mut out = Vec::new();
        if !dir.exists() {
            return Ok(out);
        }
        for entry in walkdir::WalkDir::new(dir)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            if entry.file_type().is_file()
                && entry.path().extension().map(|e| e == "md").unwrap_or(false)
            {
                if let Some(rel) = self.rel(entry.path()) {
                    out.push(rel);
                }
            }
        }
        out.sort();
        Ok(out)
    }

    /// Load and parse a page. `input` is canonicalized (see
    /// [`Workspace::canonical_page_path`]); if the canonical path is missing,
    /// the raw input is retried as-is (so `raw/foo.md` also works).
    pub fn load_page(&self, input: &str) -> Result<(String, Page)> {
        let canon = self.canonical_page_path(input);
        let abs = self.abs(&canon)?;
        if abs.exists() {
            let raw = self.read_file_abs(&abs)?;
            let page = Page::parse(&raw, &canon)?;
            return Ok((canon, page));
        }
        // Fall back to the input path verbatim.
        let verbatim = input.trim().trim_start_matches("./").replace('\\', "/");
        let abs2 = self.abs(&verbatim)?;
        if abs2.exists() {
            let raw = self.read_file_abs(&abs2)?;
            return Ok((verbatim.clone(), Page::parse(&raw, &verbatim)?));
        }
        Err(WikiError::PageNotFound(canon))
    }

    /// Read a page into a [`PageView`] including resolved backlinks.
    pub fn read_page(&self, input: &str) -> Result<PageView> {
        let (path, page) = self.load_page(input)?;
        let outbound: Vec<String> = extract_wikilinks(&page.body)
            .into_iter()
            .map(|l| l.target)
            .collect();
        let graph = self.build_link_graph()?;
        let backlinks = graph.backlinks(&path).to_vec();
        Ok(PageView {
            frontmatter: FrontmatterView::from(&page.frontmatter),
            body: page.body,
            outbound_links: outbound,
            backlinks,
            path,
        })
    }

    /// Write (create or overwrite) a page. Returns the canonical path written.
    pub fn write_page(&self, input: &str, page: &Page) -> Result<String> {
        let canon = self.canonical_page_path(input);
        let abs = self.abs(&canon)?;
        let contents = page.render()?;
        self.write_raw_abs(&abs, &contents)?;
        Ok(canon)
    }

    /// Patch a page in place under a given heading, appending or replacing the
    /// section body. Returns the canonical path.
    pub fn edit_page(
        &self,
        input: &str,
        heading: &str,
        content: &str,
        mode: EditMode,
    ) -> Result<String> {
        let (path, mut page) = self.load_page(input)?;
        page.body = patch_section(&page.body, heading, content, mode).ok_or_else(|| {
            WikiError::HeadingNotFound {
                page: path.clone(),
                heading: heading.to_string(),
            }
        })?;
        // Refresh the `updated` frontmatter stamp.
        page.frontmatter.updated = today();
        let abs = self.abs(&path)?;
        self.write_raw_abs(&abs, &page.render()?)?;
        Ok(path)
    }

    // --- link graph -------------------------------------------------------

    /// Build the whole-wiki link graph, resolving `[[wikilinks]]` to existing
    /// page paths (dangling links are recorded separately).
    pub fn build_link_graph(&self) -> Result<LinkGraph> {
        let pages = self.list_pages()?;
        let mut titles: BTreeMap<String, String> = BTreeMap::new();
        let mut bodies: Vec<(String, String)> = Vec::with_capacity(pages.len());

        for p in &pages {
            let raw = self.read_file(p)?;
            let page = Page::parse(&raw, p)?;
            titles.insert(p.clone(), page.effective_title());
            bodies.push((p.clone(), page.body));
        }

        let mut graph = LinkGraph::new();
        for p in &pages {
            graph.touch(p);
        }
        for (from, body) in &bodies {
            for link in extract_wikilinks(body) {
                let resolved = resolve_link_target(&link.target, &pages, &titles);
                graph.add_edge(from, &link.target, resolved.as_deref());
            }
        }
        Ok(graph)
    }
}

/// Resolve a raw `[[wikilink]]` target to an existing page path.
///
/// Strategy (first match wins): exact path (with/without `wiki/` prefix and
/// `.md`), then by file stem (basename), then by page title (case-insensitive).
pub fn resolve_link_target(
    target: &str,
    pages: &[String],
    titles: &BTreeMap<String, String>,
) -> Option<String> {
    let t = target.trim();
    if t.is_empty() {
        return None;
    }

    // Candidate 1: treat as a path.
    let as_path = if t.ends_with(".md") {
        t.to_string()
    } else {
        format!("{t}.md")
    };
    let with_wiki = if as_path.starts_with("wiki/") || as_path.starts_with("raw/") {
        as_path.clone()
    } else {
        format!("wiki/{as_path}")
    };
    if pages.iter().any(|p| p == &with_wiki) {
        return Some(with_wiki);
    }
    if pages.iter().any(|p| p == &as_path) {
        return Some(as_path);
    }

    // Candidate 2: by file stem (basename without extension).
    let stem_target = t.rsplit('/').next().unwrap_or(t).trim_end_matches(".md");
    for p in pages {
        if page_stem(p) == stem_target {
            return Some(p.clone());
        }
    }

    // Candidate 3: by title (case-insensitive).
    let lower = t.to_lowercase();
    for (p, title) in titles {
        if !title.is_empty() && title.to_lowercase() == lower {
            return Some(p.clone());
        }
    }
    None
}

/// The file stem of a workspace-relative page path (`wiki/entities/ada.md` -> `ada`).
pub fn page_stem(path: &str) -> &str {
    path.rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".md")
}

/// Apply an [`EditMode`] patch to the section under `heading` within `body`.
///
/// A section spans from the matched heading line to the next heading of the same
/// or higher level (or end of document). Returns `None` if the heading isn't found.
fn patch_section(body: &str, heading: &str, content: &str, mode: EditMode) -> Option<String> {
    let lines: Vec<&str> = body.lines().collect();
    let target = heading.trim();

    let mut start: Option<usize> = None;
    let mut level = 0usize;
    for (i, line) in lines.iter().enumerate() {
        if let Some((lvl, text)) = parse_heading(line) {
            if text == target {
                start = Some(i);
                level = lvl;
                break;
            }
        }
    }
    let start = start?;

    // Find the end of the section: next heading with level <= this one.
    let mut end = lines.len();
    for (i, line) in lines.iter().enumerate().skip(start + 1) {
        if let Some((lvl, _)) = parse_heading(line) {
            if lvl <= level {
                end = i;
                break;
            }
        }
    }

    let before = &lines[..=start];
    let section_body = &lines[start + 1..end];
    let after = &lines[end..];

    let mut result: Vec<String> = before.iter().map(|s| s.to_string()).collect();
    match mode {
        EditMode::Replace => {
            // Keep a blank line after the heading, then the new content.
            result.push(String::new());
            for l in content.trim_end().lines() {
                result.push(l.to_string());
            }
        }
        EditMode::Append => {
            // Keep existing body, trim trailing blanks, then append.
            let mut kept: Vec<String> = section_body.iter().map(|s| s.to_string()).collect();
            while kept.last().map(|s| s.trim().is_empty()).unwrap_or(false) {
                kept.pop();
            }
            result.extend(kept);
            result.push(String::new());
            for l in content.trim_end().lines() {
                result.push(l.to_string());
            }
        }
    }
    // Blank line before the next section, if any.
    if !after.is_empty() {
        result.push(String::new());
        result.extend(after.iter().map(|s| s.to_string()));
    }

    let mut out = result.join("\n");
    if !out.ends_with('\n') {
        out.push('\n');
    }
    Some(out)
}

/// Parse an ATX heading line into `(level, text)`.
fn parse_heading(line: &str) -> Option<(usize, &str)> {
    let t = line.trim_start();
    if !t.starts_with('#') {
        return None;
    }
    let hashes = t.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = t[hashes..].trim();
    // Require a space after the hashes for a valid ATX heading.
    if !t[hashes..].starts_with(' ') && !rest.is_empty() {
        return None;
    }
    Some((hashes, rest))
}

/// Today's date as `YYYY-MM-DD` in local time.
pub fn today() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

// --- templates -----------------------------------------------------------

const GITIGNORE_TEMPLATE: &str = "# tantivy full-text index (rebuildable from markdown)\n.wiki/\n\n# OS / editor cruft\n.DS_Store\n";

const LOG_TEMPLATE: &str = "---\ntype: log\ntitle: Log\n---\n\n# Log\n\nAppend-only, chronological record of ingests, queries, and lint passes.\nEach entry starts with `## [YYYY-MM-DD] <op> | <title>` so the log stays\ngreppable, e.g. `grep \"^## \\[\" wiki/log.md | tail -5`.\n";

fn overview_template(today: &str) -> String {
    format!(
        "---\ntype: overview\ntitle: Overview\ncreated: {today}\nupdated: {today}\nstatus: draft\n---\n\n# Overview\n\n_Top-level synthesis / evolving thesis for this wiki. The agent keeps this page\ncurrent as sources are ingested — a running summary of what the wiki knows and\nthe central questions it is tracking. Replace this placeholder as content grows._\n"
    )
}

fn index_template(today: &str) -> String {
    format!(
        "---\ntype: index\ntitle: Index\nupdated: {today}\n---\n\n# Index\n\nCatalog of every page, grouped by category, each with a one-line summary.\nRegenerated by `wiki index` / the `wiki_index` tool from page frontmatter.\n\n## Entities\n\n## Concepts\n\n## Sources\n"
    )
}

/// The schema file. This is the most important configuration in the whole
/// system: it turns a generic agent into a disciplined wiki maintainer.
const AGENTS_TEMPLATE: &str = include_str!("templates/AGENTS.md");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Frontmatter;

    fn tmp() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn init_creates_three_layer_layout() {
        let dir = tmp();
        let (ws, _git) = Workspace::init(dir.path()).unwrap();
        assert!(ws.agents_file().exists());
        assert!(ws.raw_dir().exists());
        assert!(ws.wiki_dir().join("overview.md").exists());
        assert!(ws.wiki_dir().join("index.md").exists());
        assert!(ws.wiki_dir().join("log.md").exists());
        assert!(ws.wiki_dir().join("entities").exists());
        assert!(ws.root().join(".gitignore").exists());
        // Re-init should refuse.
        assert!(Workspace::init(dir.path()).is_err());
        // Open should succeed.
        assert!(Workspace::open(dir.path()).is_ok());
    }

    #[test]
    fn canonical_page_path_forms() {
        let ws = Workspace::open_unchecked("/tmp/x");
        assert_eq!(
            ws.canonical_page_path("entities/ada"),
            "wiki/entities/ada.md"
        );
        assert_eq!(
            ws.canonical_page_path("entities/ada.md"),
            "wiki/entities/ada.md"
        );
        assert_eq!(
            ws.canonical_page_path("wiki/concepts/x.md"),
            "wiki/concepts/x.md"
        );
        assert_eq!(ws.canonical_page_path("overview"), "wiki/overview.md");
        assert_eq!(ws.canonical_page_path("raw/foo.md"), "raw/foo.md");
    }

    #[test]
    fn rejects_parent_dir_escape() {
        let ws = Workspace::open_unchecked("/tmp/x");
        assert!(ws.abs("../etc/passwd").is_err());
    }

    #[test]
    fn write_read_roundtrip_with_backlinks() {
        let dir = tmp();
        let (ws, _) = Workspace::init(dir.path()).unwrap();

        let fm = Frontmatter {
            kind: "entity".into(),
            title: "Ada".into(),
            ..Default::default()
        };
        ws.write_page(
            "entities/ada",
            &Page::new(fm, "Ada links to [[concepts/engine]].\n"),
        )
        .unwrap();

        let fm2 = Frontmatter {
            kind: "concept".into(),
            title: "Engine".into(),
            ..Default::default()
        };
        ws.write_page("concepts/engine", &Page::new(fm2, "The engine.\n"))
            .unwrap();

        let view = ws.read_page("concepts/engine").unwrap();
        assert_eq!(view.path, "wiki/concepts/engine.md");
        assert_eq!(view.backlinks, vec!["wiki/entities/ada.md".to_string()]);

        let ada = ws.read_page("entities/ada").unwrap();
        assert_eq!(ada.outbound_links, vec!["concepts/engine".to_string()]);
    }

    #[test]
    fn edit_append_and_replace() {
        let dir = tmp();
        let (ws, _) = Workspace::init(dir.path()).unwrap();
        let fm = Frontmatter::default();
        ws.write_page(
            "concepts/x",
            &Page::new(fm, "# X\n\n## Details\n\nOriginal.\n\n## Refs\n\n- a\n"),
        )
        .unwrap();

        ws.edit_page("concepts/x", "Details", "Appended line.", EditMode::Append)
            .unwrap();
        let v = ws.read_page("concepts/x").unwrap();
        assert!(v.body.contains("Original."));
        assert!(v.body.contains("Appended line."));
        // Refs section must survive.
        assert!(v.body.contains("## Refs"));

        ws.edit_page("concepts/x", "Details", "Brand new.", EditMode::Replace)
            .unwrap();
        let v2 = ws.read_page("concepts/x").unwrap();
        assert!(!v2.body.contains("Original."));
        assert!(v2.body.contains("Brand new."));
        assert!(v2.body.contains("## Refs"));
    }

    #[test]
    fn edit_missing_heading_errors() {
        let dir = tmp();
        let (ws, _) = Workspace::init(dir.path()).unwrap();
        ws.write_page(
            "concepts/x",
            &Page::new(Frontmatter::default(), "# X\n\nBody\n"),
        )
        .unwrap();
        assert!(ws
            .edit_page("concepts/x", "Nope", "y", EditMode::Append)
            .is_err());
    }

    #[test]
    fn link_resolution_by_stem_and_title() {
        let pages = vec![
            "wiki/entities/ada.md".to_string(),
            "wiki/concepts/analytical-engine.md".to_string(),
        ];
        let mut titles = BTreeMap::new();
        titles.insert(
            "wiki/concepts/analytical-engine.md".to_string(),
            "Analytical Engine".to_string(),
        );
        // By explicit path.
        assert_eq!(
            resolve_link_target("entities/ada", &pages, &titles).as_deref(),
            Some("wiki/entities/ada.md")
        );
        // By stem.
        assert_eq!(
            resolve_link_target("ada", &pages, &titles).as_deref(),
            Some("wiki/entities/ada.md")
        );
        // By title.
        assert_eq!(
            resolve_link_target("Analytical Engine", &pages, &titles).as_deref(),
            Some("wiki/concepts/analytical-engine.md")
        );
        // Dangling.
        assert_eq!(resolve_link_target("ghost", &pages, &titles), None);
    }
}
