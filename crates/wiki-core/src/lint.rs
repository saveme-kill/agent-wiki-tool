//! Wiki health check ("lint").
//!
//! Deterministic structural checks over the wiki. The tool surfaces problems;
//! the agent decides how to fix them (create a page, add a cross-reference,
//! re-review a stale page, etc.). Checks implemented:
//!
//! - **dead-link**: a `[[wikilink]]` whose target resolves to no page.
//! - **orphan**: a content page with no inbound links (ignoring meta pages).
//! - **stale**: a page whose cited raw source was modified after the page's
//!   `updated` date (newer data may supersede the page).
//! - **missing-source-page**: a raw source with no summary page in `wiki/sources/`.
//! - **missing-origin**: a `type: source` page with neither `origin` nor
//!   `sources` set (its provenance is untraceable).
//! - **empty-frontmatter**: a page missing a title (weakens the index/graph).

use std::collections::BTreeSet;

use serde::Serialize;

use crate::error::Result;
use crate::page::Page;
use crate::workspace::{page_stem, Workspace};

/// The category of a lint issue.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum LintKind {
    DeadLink,
    Orphan,
    Stale,
    MissingSourcePage,
    MissingOrigin,
    EmptyFrontmatter,
}

impl LintKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            LintKind::DeadLink => "dead-link",
            LintKind::Orphan => "orphan",
            LintKind::Stale => "stale",
            LintKind::MissingSourcePage => "missing-source-page",
            LintKind::MissingOrigin => "missing-origin",
            LintKind::EmptyFrontmatter => "empty-frontmatter",
        }
    }
}

/// A single issue found by [`lint`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LintIssue {
    pub kind: LintKind,
    /// The page or source the issue is about (workspace-relative path).
    pub subject: String,
    /// Human-readable description.
    pub message: String,
}

/// The full lint report.
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct LintReport {
    pub issues: Vec<LintIssue>,
}

impl LintReport {
    pub fn is_clean(&self) -> bool {
        self.issues.is_empty()
    }

    /// Count issues of a given kind.
    pub fn count(&self, kind: LintKind) -> usize {
        self.issues.iter().filter(|i| i.kind == kind).count()
    }
}

/// Meta pages that are exempt from orphan/frontmatter checks.
fn is_meta_page(path: &str) -> bool {
    matches!(page_stem(path), "index" | "log" | "overview")
}

/// Run all structural checks over the workspace.
pub fn lint(ws: &Workspace) -> Result<LintReport> {
    let mut report = LintReport::default();
    let pages = ws.list_pages()?;
    let graph = ws.build_link_graph()?;

    // --- dead links -------------------------------------------------------
    for (page, target) in graph.all_dangling() {
        report.issues.push(LintIssue {
            kind: LintKind::DeadLink,
            subject: page.clone(),
            message: format!("[[{target}]] does not resolve to any page"),
        });
    }

    // --- orphans ----------------------------------------------------------
    // A page is an orphan if nothing links to it *except* the auto-generated
    // catalog pages (index.md / log.md), which mechanically link to everything
    // and therefore can't count as a real cross-reference.
    for page in &pages {
        if is_meta_page(page) {
            continue;
        }
        let real_backlinks = graph
            .backlinks(page)
            .iter()
            .filter(|src| !is_meta_page(src))
            .count();
        if real_backlinks == 0 {
            report.issues.push(LintIssue {
                kind: LintKind::Orphan,
                subject: page.clone(),
                message: "no inbound links; add a cross-reference from a related page".into(),
            });
        }
    }

    // --- empty frontmatter + stale + gather referenced sources ------------
    let mut referenced_sources: BTreeSet<String> = BTreeSet::new();
    for page in &pages {
        if is_meta_page(page) {
            continue;
        }
        let raw = ws.read_file(page)?;
        let parsed = Page::parse(&raw, page)?;

        if parsed.effective_title().is_empty() {
            report.issues.push(LintIssue {
                kind: LintKind::EmptyFrontmatter,
                subject: page.clone(),
                message: "page has no title in frontmatter or heading".into(),
            });
        }

        // A source-summary page should record where its material came from —
        // either an upstream `origin:` URL or a `sources:` raw file (ideally
        // both). If it has neither, its provenance is untraceable.
        if parsed.frontmatter.kind == "source"
            && parsed.frontmatter.origin.trim().is_empty()
            && parsed.frontmatter.sources.is_empty()
        {
            report.issues.push(LintIssue {
                kind: LintKind::MissingOrigin,
                subject: page.clone(),
                message: "source page has no `origin:` URL or `sources:` raw file; \
                          record where its material came from"
                    .into(),
            });
        }

        for src in &parsed.frontmatter.sources {
            let src_rel = normalize_source_ref(src);
            referenced_sources.insert(src_rel.clone());
            // Stale check: source file mtime newer than page `updated`.
            if let Some(msg) = stale_message(ws, &src_rel, &parsed.frontmatter.updated) {
                report.issues.push(LintIssue {
                    kind: LintKind::Stale,
                    subject: page.clone(),
                    message: msg,
                });
            }
        }
    }

    // --- missing source pages --------------------------------------------
    // Any file in raw/ that isn't referenced by some page's `sources:` and has
    // no obvious summary page is flagged.
    let raw_files = ws.list_raw()?;
    for raw in &raw_files {
        if referenced_sources.contains(raw) {
            continue;
        }
        report.issues.push(LintIssue {
            kind: LintKind::MissingSourcePage,
            subject: raw.clone(),
            message: "raw source has no summary page (no page lists it under `sources:`)".into(),
        });
    }

    Ok(report)
}

/// Normalize a `sources:` reference to a `raw/...` path for comparison.
fn normalize_source_ref(src: &str) -> String {
    let clean = src.trim().trim_start_matches("./").replace('\\', "/");
    if clean.starts_with("raw/") {
        clean
    } else {
        format!("raw/{clean}")
    }
}

/// If the source file's mtime is newer than the page's `updated` date, produce a
/// staleness message. Best-effort: returns `None` if either is unavailable.
fn stale_message(ws: &Workspace, src_rel: &str, updated: &str) -> Option<String> {
    if updated.trim().is_empty() {
        return None;
    }
    let abs = ws.abs(src_rel).ok()?;
    let meta = std::fs::metadata(&abs).ok()?;
    let modified = meta.modified().ok()?;
    let modified: chrono::DateTime<chrono::Local> = modified.into();
    let src_date = modified.format("%Y-%m-%d").to_string();
    // String compare works for ISO `YYYY-MM-DD`.
    if src_date.as_str() > updated {
        Some(format!(
            "source {src_rel} modified {src_date}, newer than page updated {updated}; re-review"
        ))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Frontmatter;
    use crate::workspace::Workspace;

    fn init_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let (ws, _) = Workspace::init(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn detects_dead_link_and_orphan() {
        let (_d, ws) = init_ws();

        // ada links to a nonexistent page -> dead link. ada has no inbound -> orphan.
        let mut fm = Frontmatter::default();
        fm.kind = "entity".into();
        fm.title = "Ada".into();
        ws.write_page(
            "entities/ada",
            &Page::new(fm, "See [[concepts/ghost]] which does not exist.\n"),
        )
        .unwrap();

        let report = lint(&ws).unwrap();
        assert_eq!(report.count(LintKind::DeadLink), 1);
        assert!(report.count(LintKind::Orphan) >= 1);
        assert!(report
            .issues
            .iter()
            .any(|i| i.kind == LintKind::DeadLink && i.message.contains("concepts/ghost")));
    }

    #[test]
    fn linked_pages_are_not_orphans() {
        let (_d, ws) = init_ws();
        let mut a = Frontmatter::default();
        a.title = "A".into();
        ws.write_page("entities/a", &Page::new(a, "Links to [[entities/b]].\n"))
            .unwrap();
        let mut b = Frontmatter::default();
        b.title = "B".into();
        ws.write_page(
            "entities/b",
            &Page::new(b, "Links back to [[entities/a]].\n"),
        )
        .unwrap();

        let report = lint(&ws).unwrap();
        assert_eq!(report.count(LintKind::Orphan), 0);
        assert_eq!(report.count(LintKind::DeadLink), 0);
    }

    #[test]
    fn flags_missing_source_page() {
        let (_d, ws) = init_ws();
        crate::ingest::raw_add(&ws, "unread.md", "some source text", false).unwrap();

        let report = lint(&ws).unwrap();
        assert_eq!(report.count(LintKind::MissingSourcePage), 1);
        assert_eq!(
            report
                .issues
                .iter()
                .find(|i| i.kind == LintKind::MissingSourcePage)
                .unwrap()
                .subject,
            "raw/unread.md"
        );
    }

    #[test]
    fn referenced_source_not_flagged_missing() {
        let (_d, ws) = init_ws();
        crate::ingest::raw_add(&ws, "read.md", "text", false).unwrap();
        let mut fm = Frontmatter::default();
        fm.kind = "source".into();
        fm.title = "Summary".into();
        fm.sources = vec!["raw/read.md".into()];
        // Link it so it isn't an orphan; link target must exist to avoid dead-link.
        ws.write_page("sources/read", &Page::new(fm, "Summary of the source.\n"))
            .unwrap();

        let report = lint(&ws).unwrap();
        assert_eq!(report.count(LintKind::MissingSourcePage), 0);
    }

    #[test]
    fn flags_source_page_missing_origin_and_sources() {
        let (_d, ws) = init_ws();
        let mut fm = Frontmatter::default();
        fm.kind = "source".into();
        fm.title = "Untraceable".into();
        // Neither origin nor sources set -> provenance is untraceable.
        ws.write_page(
            "sources/mystery",
            &Page::new(fm, "A summary from nowhere.\n"),
        )
        .unwrap();

        let report = lint(&ws).unwrap();
        assert_eq!(report.count(LintKind::MissingOrigin), 1);
        assert_eq!(
            report
                .issues
                .iter()
                .find(|i| i.kind == LintKind::MissingOrigin)
                .unwrap()
                .subject,
            "wiki/sources/mystery.md"
        );
    }

    #[test]
    fn source_page_with_origin_is_not_missing_origin() {
        let (_d, ws) = init_ws();
        let mut fm = Frontmatter::default();
        fm.kind = "source".into();
        fm.title = "Feishu Doc".into();
        fm.origin = "https://x.feishu.cn/docx/abc".into();
        ws.write_page(
            "sources/feishu",
            &Page::new(fm, "Summary of a Feishu doc.\n"),
        )
        .unwrap();

        let report = lint(&ws).unwrap();
        assert_eq!(report.count(LintKind::MissingOrigin), 0);
    }

    #[test]
    fn non_source_page_without_origin_is_not_flagged() {
        let (_d, ws) = init_ws();
        // A concept page has no provenance obligation.
        let mut fm = Frontmatter::default();
        fm.kind = "concept".into();
        fm.title = "An Idea".into();
        ws.write_page("concepts/idea", &Page::new(fm, "Just an idea.\n"))
            .unwrap();

        let report = lint(&ws).unwrap();
        assert_eq!(report.count(LintKind::MissingOrigin), 0);
    }
}
