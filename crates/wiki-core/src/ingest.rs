//! Ingest support: register raw sources into `raw/` and read them back in
//! title-delimited chunks for the agent to review.
//!
//! These are the deterministic primitives behind the Ingest workflow. The agent
//! does the reading, synthesis, and page-writing; these functions just move
//! bytes into the immutable `raw/` layer and hand them back in digestible pieces.

use serde::Serialize;

use crate::error::{Result, WikiError};
use crate::workspace::Workspace;

/// A chunk of a raw source returned by [`raw_read`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Chunk {
    /// 0-based chunk index.
    pub index: usize,
    /// Total number of chunks in the source.
    pub total: usize,
    /// Heading this chunk falls under, if the source is markdown with headings.
    pub heading: Option<String>,
    /// The chunk text.
    pub text: String,
}

/// Register a new raw source under `raw/`.
///
/// `name` is the destination filename (or relative path) under `raw/`; `content`
/// is written verbatim. Returns the workspace-relative path written. Refuses to
/// overwrite an existing raw file (raw sources are immutable) unless `overwrite`.
pub fn raw_add(ws: &Workspace, name: &str, content: &str, overwrite: bool) -> Result<String> {
    let clean = name.trim().trim_start_matches("./").replace('\\', "/");
    // Strip a leading `raw/` if the caller included it, then re-root under raw/.
    let clean = clean.strip_prefix("raw/").unwrap_or(&clean);
    if clean.is_empty() || clean.split('/').any(|c| c == ".." || c.is_empty()) {
        return Err(WikiError::InvalidPagePath {
            path: name.to_string(),
            reason: "invalid raw source name".into(),
        });
    }
    let rel = format!("raw/{clean}");
    let abs = ws.abs(&rel)?;
    if abs.exists() && !overwrite {
        return Err(WikiError::Other(format!(
            "raw source already exists (raw is immutable): {rel}"
        )));
    }
    if let Some(parent) = abs.parent() {
        std::fs::create_dir_all(parent).map_err(|e| WikiError::io(parent, e))?;
    }
    std::fs::write(&abs, content).map_err(|e| WikiError::io(&abs, e))?;
    Ok(rel)
}

/// Read a raw source back in chunks for review.
///
/// For markdown sources, chunks are split on top-level headings so each chunk is
/// a coherent section. For very long headingless text, chunks are split on a
/// character budget. `max_chars` caps the size of each chunk (a section larger
/// than the budget is further subdivided).
pub fn raw_read(ws: &Workspace, name: &str, max_chars: usize) -> Result<Vec<Chunk>> {
    let rel = normalize_raw_rel(name);
    let abs = ws.abs(&rel)?;
    if !abs.exists() {
        return Err(WikiError::RawNotFound(rel));
    }
    let text = std::fs::read_to_string(&abs).map_err(|e| WikiError::io(&abs, e))?;
    let budget = max_chars.max(200);

    let sections = split_sections(&text);
    let mut raw_chunks: Vec<(Option<String>, String)> = Vec::new();
    for (heading, body) in sections {
        if body.trim().is_empty() && heading.is_none() {
            continue;
        }
        for piece in split_on_budget(&body, budget) {
            raw_chunks.push((heading.clone(), piece));
        }
    }
    if raw_chunks.is_empty() {
        raw_chunks.push((None, text));
    }

    let total = raw_chunks.len();
    Ok(raw_chunks
        .into_iter()
        .enumerate()
        .map(|(index, (heading, text))| Chunk {
            index,
            total,
            heading,
            text,
        })
        .collect())
}

/// Normalize a raw source identifier to a `raw/...` workspace-relative path.
fn normalize_raw_rel(name: &str) -> String {
    let clean = name.trim().trim_start_matches("./").replace('\\', "/");
    if clean.starts_with("raw/") {
        clean
    } else {
        format!("raw/{clean}")
    }
}

/// Split markdown into `(heading, section_body)` pairs on ATX headings. Content
/// before the first heading is returned with a `None` heading.
fn split_sections(text: &str) -> Vec<(Option<String>, String)> {
    let mut sections: Vec<(Option<String>, String)> = Vec::new();
    let mut current_heading: Option<String> = None;
    let mut current: String = String::new();

    for line in text.lines() {
        let t = line.trim_start();
        let is_heading = t.starts_with('#')
            && t.trim_start_matches('#').starts_with(' ')
            && t.chars().take_while(|&c| c == '#').count() <= 6;
        if is_heading {
            if !current.trim().is_empty() || current_heading.is_some() {
                sections.push((current_heading.take(), std::mem::take(&mut current)));
            }
            let heading_text = t.trim_start_matches('#').trim().to_string();
            current_heading = Some(heading_text);
            // Keep the heading line itself in the section text for context.
            current.push_str(line);
            current.push('\n');
        } else {
            current.push_str(line);
            current.push('\n');
        }
    }
    if !current.trim().is_empty() || current_heading.is_some() {
        sections.push((current_heading, current));
    }
    if sections.is_empty() {
        sections.push((None, text.to_string()));
    }
    sections
}

/// Split a string into pieces no larger than `budget` characters, preferring to
/// break on paragraph (blank-line) boundaries.
fn split_on_budget(text: &str, budget: usize) -> Vec<String> {
    if text.chars().count() <= budget {
        return vec![text.to_string()];
    }
    let mut pieces = Vec::new();
    let mut current = String::new();
    for para in text.split("\n\n") {
        let candidate_len = current.chars().count() + para.chars().count() + 2;
        if !current.is_empty() && candidate_len > budget {
            pieces.push(std::mem::take(&mut current));
        }
        if para.chars().count() > budget {
            // A single oversized paragraph: hard-split on char boundary.
            if !current.is_empty() {
                pieces.push(std::mem::take(&mut current));
            }
            let chars: Vec<char> = para.chars().collect();
            for window in chars.chunks(budget) {
                pieces.push(window.iter().collect());
            }
        } else {
            if !current.is_empty() {
                current.push_str("\n\n");
            }
            current.push_str(para);
        }
    }
    if !current.trim().is_empty() {
        pieces.push(current);
    }
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    fn init_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let (ws, _) = Workspace::init(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn add_then_read_chunks_by_heading() {
        let (_d, ws) = init_ws();
        let content = "# Intro\n\nFirst section.\n\n# Details\n\nSecond section.\n";
        let rel = raw_add(&ws, "article.md", content, false).unwrap();
        assert_eq!(rel, "raw/article.md");

        let chunks = raw_read(&ws, "article.md", 1000).unwrap();
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].heading.as_deref(), Some("Intro"));
        assert!(chunks[0].text.contains("First section"));
        assert_eq!(chunks[1].heading.as_deref(), Some("Details"));
        assert_eq!(chunks[1].total, 2);
    }

    #[test]
    fn raw_is_immutable_without_overwrite() {
        let (_d, ws) = init_ws();
        raw_add(&ws, "a.md", "one", false).unwrap();
        assert!(raw_add(&ws, "a.md", "two", false).is_err());
        assert!(raw_add(&ws, "a.md", "two", true).is_ok());
        assert_eq!(ws.read_file("raw/a.md").unwrap(), "two");
    }

    #[test]
    fn read_missing_raw_errors() {
        let (_d, ws) = init_ws();
        assert!(raw_read(&ws, "nope.md", 1000).is_err());
    }

    #[test]
    fn long_section_is_subdivided() {
        let (_d, ws) = init_ws();
        let big = "para one.\n\n".to_string() + &"x".repeat(500) + "\n\nlast para.";
        raw_add(&ws, "big.md", &big, false).unwrap();
        let chunks = raw_read(&ws, "big.md", 200).unwrap();
        assert!(chunks.len() >= 3);
        assert!(chunks.iter().all(|c| c.text.chars().count() <= 200 + 4));
    }
}
