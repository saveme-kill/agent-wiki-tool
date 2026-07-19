//! Wiki pages: YAML frontmatter + markdown body.
//!
//! Pages are Obsidian-compatible: a `---`-delimited YAML frontmatter block
//! (`type`, `title`, `tags`, `sources`, `created`, `updated`, `status`) followed
//! by markdown that may contain `[[wikilink]]` cross-references. This mirrors the
//! llm-wiki convention so the same files work in Obsidian (graph view, Dataview).

use serde::{Deserialize, Serialize};

use crate::error::{Result, WikiError};

/// Structured YAML frontmatter carried at the top of every wiki page.
///
/// Unknown keys are preserved round-trip via [`Frontmatter::extra`] so an agent
/// can add domain-specific metadata without the tooling dropping it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Frontmatter {
    /// Page kind: `entity`, `concept`, `source`, `overview`, `index`, `log`, …
    /// Serialized as `type` to match Obsidian/Dataview conventions.
    #[serde(rename = "type", default = "default_type")]
    pub kind: String,

    /// Human-readable page title.
    #[serde(default)]
    pub title: String,

    /// Free-form tags.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    /// Slugs/paths of raw sources this page draws from (e.g. `article-foo`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,

    /// Upstream origin of this page's material: the canonical external URL it was
    /// derived from (e.g. a Feishu doc). A first-class field (not just `extra`) so
    /// `wiki_read` can surface it and `wiki_lint` can flag sources that lack it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub origin: String,

    /// `YYYY-MM-DD` creation date.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub created: String,

    /// `YYYY-MM-DD` last-updated date.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub updated: String,

    /// Editorial status: `draft`, `stable`, `stub`, `needs-review`, …
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub status: String,

    /// Any additional frontmatter keys, preserved verbatim on round-trip.
    #[serde(flatten)]
    pub extra: serde_yaml::Mapping,
}

fn default_type() -> String {
    "note".to_string()
}

impl Default for Frontmatter {
    fn default() -> Self {
        Frontmatter {
            kind: default_type(),
            title: String::new(),
            tags: Vec::new(),
            sources: Vec::new(),
            origin: String::new(),
            created: String::new(),
            updated: String::new(),
            status: String::new(),
            extra: serde_yaml::Mapping::new(),
        }
    }
}

impl Frontmatter {
    /// A one-line summary of the page derived from frontmatter + first body line.
    /// Used by the index builder.
    pub fn summarize(&self, body: &str) -> String {
        // First non-empty, non-heading line of the body, trimmed to one line.
        for line in body.lines() {
            let t = line.trim();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            // Strip a leading blockquote / list marker for a cleaner summary.
            let t = t.trim_start_matches(['>', '-', '*', '+']).trim();
            if !t.is_empty() {
                return one_line(t);
            }
        }
        String::new()
    }
}

/// Collapse to a single line and cap the length for index summaries.
fn one_line(s: &str) -> String {
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    const MAX: usize = 200;
    if collapsed.chars().count() > MAX {
        let truncated: String = collapsed.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

/// A parsed wiki page: frontmatter + markdown body.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub frontmatter: Frontmatter,
    /// Markdown body (everything after the frontmatter block).
    pub body: String,
}

impl Page {
    /// Build a new page with the given frontmatter and body.
    pub fn new(frontmatter: Frontmatter, body: impl Into<String>) -> Self {
        Page {
            frontmatter,
            body: body.into(),
        }
    }

    /// Parse raw file contents into frontmatter + body.
    ///
    /// A leading `---\n … \n---` block is parsed as YAML frontmatter. If there is
    /// no frontmatter block, the whole text is treated as the body with default
    /// frontmatter (title inferred from the first `# ` heading, if any).
    pub fn parse(raw: &str, context_path: &str) -> Result<Page> {
        match split_frontmatter(raw) {
            Some((yaml, body)) => {
                let frontmatter: Frontmatter =
                    serde_yaml::from_str(yaml).map_err(|e| WikiError::Frontmatter {
                        path: context_path.to_string(),
                        reason: e.to_string(),
                    })?;
                Ok(Page {
                    frontmatter,
                    body: body.to_string(),
                })
            }
            None => {
                let mut fm = Frontmatter::default();
                if let Some(title) = first_heading(raw) {
                    fm.title = title;
                }
                Ok(Page {
                    frontmatter: fm,
                    body: raw.to_string(),
                })
            }
        }
    }

    /// Render the page back to file contents (frontmatter block + body).
    pub fn render(&self) -> Result<String> {
        let yaml = serde_yaml::to_string(&self.frontmatter)?;
        // serde_yaml already ends with a newline; ensure the body is separated.
        let body = self.body.trim_start_matches('\n');
        Ok(format!("---\n{yaml}---\n\n{body}"))
    }

    /// The effective title: frontmatter title, else first heading, else empty.
    pub fn effective_title(&self) -> String {
        if !self.frontmatter.title.is_empty() {
            self.frontmatter.title.clone()
        } else {
            first_heading(&self.body).unwrap_or_default()
        }
    }
}

/// A read-oriented view of a page returned by `wiki_read`: the page plus its
/// resolved outbound links and inbound backlinks.
#[derive(Debug, Clone, Serialize)]
pub struct PageView {
    /// Workspace-relative path, e.g. `wiki/entities/foo.md`.
    pub path: String,
    pub frontmatter: FrontmatterView,
    pub body: String,
    /// Outbound `[[wikilinks]]` (raw targets as written in the page).
    pub outbound_links: Vec<String>,
    /// Pages that link *to* this page (workspace-relative paths).
    pub backlinks: Vec<String>,
}

/// Serializable projection of [`Frontmatter`] for tool output (kept flat and
/// JSON-friendly; `extra` is dropped to keep tool payloads predictable).
#[derive(Debug, Clone, Serialize)]
pub struct FrontmatterView {
    #[serde(rename = "type")]
    pub kind: String,
    pub title: String,
    pub tags: Vec<String>,
    pub sources: Vec<String>,
    pub origin: String,
    pub created: String,
    pub updated: String,
    pub status: String,
}

impl From<&Frontmatter> for FrontmatterView {
    fn from(f: &Frontmatter) -> Self {
        FrontmatterView {
            kind: f.kind.clone(),
            title: f.title.clone(),
            tags: f.tags.clone(),
            sources: f.sources.clone(),
            origin: f.origin.clone(),
            created: f.created.clone(),
            updated: f.updated.clone(),
            status: f.status.clone(),
        }
    }
}

/// Split a `---`-delimited leading YAML frontmatter block from the body.
/// Returns `(yaml, body)` when a well-formed block is present.
fn split_frontmatter(raw: &str) -> Option<(&str, &str)> {
    // Frontmatter must start at the very beginning of the file.
    let rest = raw.strip_prefix("---")?;
    // The delimiter line must be followed by a newline.
    let rest = rest
        .strip_prefix('\n')
        .or_else(|| rest.strip_prefix("\r\n"))?;

    // Find the closing delimiter line: a line that is exactly `---`.
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if trimmed == "---" {
            let yaml = &rest[..offset];
            let body_start = offset + line.len();
            let body = &rest[body_start..];
            // Drop a single leading blank line between frontmatter and body.
            let body = body
                .strip_prefix('\n')
                .or_else(|| body.strip_prefix("\r\n"))
                .unwrap_or(body);
            return Some((yaml, body));
        }
        offset += line.len();
    }
    None
}

/// Return the text of the first ATX (`# `) heading in `md`, if any.
fn first_heading(md: &str) -> Option<String> {
    for line in md.lines() {
        let t = line.trim_start();
        if let Some(rest) = t.strip_prefix('#') {
            let heading = rest.trim_start_matches('#').trim();
            if !heading.is_empty() {
                return Some(heading.to_string());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_frontmatter_and_body() {
        let raw = "---\ntype: entity\ntitle: Ada Lovelace\ntags: [person, math]\nstatus: stable\n---\n\nAda was a mathematician. See [[concepts/analytical-engine]].\n";
        let page = Page::parse(raw, "wiki/entities/ada.md").unwrap();
        assert_eq!(page.frontmatter.kind, "entity");
        assert_eq!(page.frontmatter.title, "Ada Lovelace");
        assert_eq!(page.frontmatter.tags, vec!["person", "math"]);
        assert_eq!(page.frontmatter.status, "stable");
        assert!(page.body.contains("mathematician"));
    }

    #[test]
    fn round_trips_render_parse() {
        let mut fm = Frontmatter::default();
        fm.kind = "concept".into();
        fm.title = "Analytical Engine".into();
        fm.tags = vec!["computing".into()];
        fm.created = "2026-07-06".into();
        let page = Page::new(fm, "A mechanical general-purpose computer.\n");

        let rendered = page.render().unwrap();
        let reparsed = Page::parse(&rendered, "x.md").unwrap();
        assert_eq!(page.frontmatter, reparsed.frontmatter);
        assert_eq!(page.body.trim(), reparsed.body.trim());
    }

    #[test]
    fn no_frontmatter_infers_title_from_heading() {
        let raw = "# Some Title\n\nBody text here.\n";
        let page = Page::parse(raw, "x.md").unwrap();
        assert_eq!(page.frontmatter.title, "Some Title");
        assert_eq!(page.body, raw);
    }

    #[test]
    fn preserves_unknown_frontmatter_keys() {
        let raw = "---\ntype: source\ntitle: Paper\nauthor: Jane Doe\nyear: 2021\n---\n\nBody\n";
        let page = Page::parse(raw, "x.md").unwrap();
        let rendered = page.render().unwrap();
        assert!(rendered.contains("author: Jane Doe"));
        assert!(rendered.contains("year: 2021"));
    }

    #[test]
    fn round_trips_origin_field() {
        let raw = "---\ntype: source\ntitle: Feishu Doc\norigin: https://x.feishu.cn/docx/abc123\nsources: [raw/feishu/x.md]\n---\n\nBody\n";
        let page = Page::parse(raw, "x.md").unwrap();
        assert_eq!(page.frontmatter.origin, "https://x.feishu.cn/docx/abc123");

        // origin survives a render/parse round-trip as a first-class field...
        let rendered = page.render().unwrap();
        assert!(rendered.contains("origin: https://x.feishu.cn/docx/abc123"));
        let reparsed = Page::parse(&rendered, "x.md").unwrap();
        assert_eq!(reparsed.frontmatter, page.frontmatter);

        // ...and it is exposed on the read-oriented view.
        let view = FrontmatterView::from(&page.frontmatter);
        assert_eq!(view.origin, "https://x.feishu.cn/docx/abc123");
    }

    #[test]
    fn empty_origin_is_omitted_when_rendering() {
        let mut fm = Frontmatter::default();
        fm.kind = "concept".into();
        fm.title = "X".into();
        let page = Page::new(fm, "body\n");
        let rendered = page.render().unwrap();
        assert!(!rendered.contains("origin:"));
    }

    #[test]
    fn summarize_picks_first_prose_line() {
        let fm = Frontmatter::default();
        let body = "# Heading\n\n> A quote.\n\nThe real first sentence.\n";
        let summary = fm.summarize(body);
        assert_eq!(summary, "A quote.");
    }

    #[test]
    fn crlf_frontmatter_is_supported() {
        let raw = "---\r\ntype: entity\r\ntitle: X\r\n---\r\n\r\nBody\r\n";
        let page = Page::parse(raw, "x.md").unwrap();
        assert_eq!(page.frontmatter.kind, "entity");
        assert_eq!(page.frontmatter.title, "X");
    }
}
