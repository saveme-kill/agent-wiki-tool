//! `[[wikilink]]` extraction and the wiki link graph (forward + backlinks).
//!
//! We parse markdown with `pulldown-cmark` to get text spans, then scan those
//! spans for Obsidian-style `[[target]]` / `[[target|alias]]` / `[[target#heading]]`
//! links. Parsing via the markdown lexer (rather than a naive regex over the raw
//! source) means `[[…]]` occurrences inside fenced code blocks are ignored, which
//! matches how Obsidian resolves links.

use std::collections::BTreeMap;

use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// A single `[[wikilink]]` occurrence found in a page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WikiLink {
    /// The link target as written, e.g. `entities/ada` or `Analytical Engine`.
    /// Any `#heading` and `|alias` parts are stripped from `target`.
    pub target: String,
    /// Optional display alias (the part after `|`).
    pub alias: Option<String>,
    /// Optional heading anchor (the part after `#`).
    pub heading: Option<String>,
}

/// Extract all `[[wikilinks]]` from markdown body text, in document order.
///
/// Links inside fenced/indented code blocks and inline code are ignored.
///
/// Implementation note: markdown parsers treat `[` specially, so we cannot rely
/// on `Event::Text` spans to preserve the `[[ ... ]]` brackets intact. Instead
/// we use pulldown-cmark's *offset* iterator purely to locate the byte ranges of
/// code blocks / inline code, then scan the raw source for `[[ ... ]]`, skipping
/// any occurrence that begins inside an excluded range.
pub fn extract_wikilinks(markdown: &str) -> Vec<WikiLink> {
    let excluded = code_ranges(markdown);
    let mut out = Vec::new();

    let bytes = markdown.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == b'[' && bytes[i + 1] == b'[' {
            if let Some(close) = markdown[i + 2..].find("]]") {
                let start = i;
                let inner = &markdown[i + 2..i + 2 + close];
                let end = i + 2 + close + 2;
                if !in_ranges(start, &excluded) {
                    if let Some(link) = parse_link_inner(inner) {
                        out.push(link);
                    }
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out
}

/// Collect byte ranges of code blocks and inline code to exclude from scanning.
fn code_ranges(markdown: &str) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut code_start: Option<usize> = None;
    let parser = Parser::new(markdown).into_offset_iter();
    for (event, range) in parser {
        match event {
            Event::Start(Tag::CodeBlock(_)) => code_start = Some(range.start),
            Event::End(TagEnd::CodeBlock) => {
                let start = code_start.take().unwrap_or(range.start);
                ranges.push((start, range.end));
            }
            // Inline code: the range covers the backticks + content.
            Event::Code(_) => ranges.push((range.start, range.end)),
            _ => {}
        }
    }
    ranges
}

/// Is byte offset `pos` within any of the excluded ranges?
fn in_ranges(pos: usize, ranges: &[(usize, usize)]) -> bool {
    ranges.iter().any(|&(s, e)| pos >= s && pos < e)
}

/// Parse the inside of a `[[ ... ]]` into a [`WikiLink`].
fn parse_link_inner(inner: &str) -> Option<WikiLink> {
    let inner = inner.trim();
    if inner.is_empty() {
        return None;
    }
    // Split alias: target#heading|alias
    let (link_part, alias) = match inner.split_once('|') {
        Some((l, a)) => (l.trim(), Some(a.trim().to_string())),
        None => (inner, None),
    };
    let (target, heading) = match link_part.split_once('#') {
        Some((t, h)) => (t.trim(), Some(h.trim().to_string())),
        None => (link_part.trim(), None),
    };
    if target.is_empty() && heading.is_none() {
        return None;
    }
    Some(WikiLink {
        target: target.to_string(),
        alias,
        heading,
    })
}

/// A resolved link graph over the wiki: forward links and their inverse.
///
/// Keys and values are workspace-relative page paths (e.g. `wiki/entities/ada.md`).
#[derive(Debug, Default, Clone)]
pub struct LinkGraph {
    /// page -> set of pages it links to (resolved, existing targets only kept
    /// separately via [`LinkGraph::unresolved`]).
    forward: BTreeMap<String, Vec<String>>,
    /// page -> set of pages that link to it.
    backward: BTreeMap<String, Vec<String>>,
    /// page -> raw link targets that could not be resolved to an existing page.
    unresolved: BTreeMap<String, Vec<String>>,
}

impl LinkGraph {
    pub fn new() -> Self {
        LinkGraph::default()
    }

    /// Record that `from` links to raw target `raw_target`, resolving it against
    /// the set of known page paths. `resolve` maps a raw target to an existing
    /// page path (or `None` if it dangles).
    pub fn add_edge(&mut self, from: &str, raw_target: &str, resolved: Option<&str>) {
        match resolved {
            Some(to) => {
                push_unique(self.forward.entry(from.to_string()).or_default(), to);
                push_unique(self.backward.entry(to.to_string()).or_default(), from);
            }
            None => {
                push_unique(
                    self.unresolved.entry(from.to_string()).or_default(),
                    raw_target,
                );
            }
        }
    }

    /// Ensure a page is present in the graph even if it has no links.
    pub fn touch(&mut self, page: &str) {
        self.forward.entry(page.to_string()).or_default();
        self.backward.entry(page.to_string()).or_default();
    }

    /// Pages that `page` links to.
    pub fn forward_links(&self, page: &str) -> &[String] {
        self.forward.get(page).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Pages that link to `page`.
    pub fn backlinks(&self, page: &str) -> &[String] {
        self.backward.get(page).map(|v| v.as_slice()).unwrap_or(&[])
    }

    /// Raw link targets from `page` that don't resolve to any existing page.
    pub fn dangling_from(&self, page: &str) -> &[String] {
        self.unresolved
            .get(page)
            .map(|v| v.as_slice())
            .unwrap_or(&[])
    }

    /// All pages known to the graph.
    pub fn pages(&self) -> impl Iterator<Item = &String> {
        self.forward.keys()
    }

    /// (page, dangling_target) pairs across the whole graph.
    pub fn all_dangling(&self) -> impl Iterator<Item = (&String, &String)> {
        self.unresolved
            .iter()
            .flat_map(|(page, targets)| targets.iter().map(move |t| (page, t)))
    }
}

fn push_unique(v: &mut Vec<String>, item: &str) {
    if !v.iter().any(|x| x == item) {
        v.push(item.to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_simple_link() {
        let links = extract_wikilinks("See [[entities/ada]] for details.");
        assert_eq!(links.len(), 1);
        assert_eq!(links[0].target, "entities/ada");
        assert!(links[0].alias.is_none());
        assert!(links[0].heading.is_none());
    }

    #[test]
    fn extracts_alias_and_heading() {
        let links = extract_wikilinks("[[concepts/engine#History|the engine]]");
        assert_eq!(links[0].target, "concepts/engine");
        assert_eq!(links[0].heading.as_deref(), Some("History"));
        assert_eq!(links[0].alias.as_deref(), Some("the engine"));
    }

    #[test]
    fn ignores_links_in_code_blocks() {
        let md = "Real [[a]].\n\n```\ncode [[not-a-link]]\n```\n\nMore [[b]].";
        let links = extract_wikilinks(md);
        let targets: Vec<_> = links.iter().map(|l| l.target.as_str()).collect();
        assert_eq!(targets, vec!["a", "b"]);
    }

    #[test]
    fn handles_multiple_links_in_one_span() {
        let links = extract_wikilinks("[[a]] and [[b]] and [[c]]");
        let targets: Vec<_> = links.iter().map(|l| l.target.clone()).collect();
        assert_eq!(targets, vec!["a", "b", "c"]);
    }

    #[test]
    fn cjk_link_targets() {
        let links = extract_wikilinks("参见 [[概念/分析机|分析机]]。");
        assert_eq!(links[0].target, "概念/分析机");
        assert_eq!(links[0].alias.as_deref(), Some("分析机"));
    }

    #[test]
    fn graph_forward_and_backlinks() {
        let mut g = LinkGraph::new();
        g.add_edge("a.md", "b", Some("b.md"));
        g.add_edge("c.md", "b", Some("b.md"));
        g.add_edge("a.md", "ghost", None);

        assert_eq!(g.forward_links("a.md"), &["b.md".to_string()]);
        let mut back = g.backlinks("b.md").to_vec();
        back.sort();
        assert_eq!(back, vec!["a.md".to_string(), "c.md".to_string()]);
        assert_eq!(g.dangling_from("a.md"), &["ghost".to_string()]);
    }
}
