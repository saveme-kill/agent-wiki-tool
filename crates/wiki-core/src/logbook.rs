//! The chronological `log.md`: an append-only record of ingests, queries, and
//! lint passes.
//!
//! Each entry starts with a consistent prefix — `## [YYYY-MM-DD] <op> | <title>` —
//! so the log stays greppable, mirroring the llm-wiki tip:
//! `grep "^## \[" wiki/log.md | tail -5`.

use serde::Serialize;

use crate::error::Result;
use crate::workspace::{today, Workspace};

/// A single parsed log entry (its header line plus the body until the next entry).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LogEntry {
    /// `YYYY-MM-DD` date parsed from the header, if present.
    pub date: String,
    /// Operation: `ingest`, `query`, `lint`, or free-form.
    pub op: String,
    /// The entry title (text after `|`).
    pub title: String,
    /// The full header line as written (`## [..] .. | ..`).
    pub header: String,
    /// Body lines between this header and the next entry (trimmed).
    pub body: String,
}

const LOG_REL: &str = "wiki/log.md";

/// Append an entry to `wiki/log.md`.
///
/// The header is formatted as `## [<date>] <op> | <title>`. If `date` is `None`,
/// today's local date is used. `detail`, if provided, becomes the entry body.
pub fn log_append(
    ws: &Workspace,
    op: &str,
    title: &str,
    detail: Option<&str>,
    date: Option<&str>,
) -> Result<LogEntry> {
    let date = date.map(|d| d.to_string()).unwrap_or_else(today);
    let header = format!("## [{date}] {op} | {title}");

    let mut existing = ws.read_file(LOG_REL).unwrap_or_default();
    if !existing.ends_with('\n') {
        existing.push('\n');
    }
    let mut block = String::new();
    block.push('\n');
    block.push_str(&header);
    block.push('\n');
    if let Some(d) = detail {
        let d = d.trim_end();
        if !d.is_empty() {
            block.push('\n');
            block.push_str(d);
            block.push('\n');
        }
    }
    existing.push_str(&block);

    let abs = ws.abs(LOG_REL)?;
    std::fs::write(&abs, existing).map_err(|e| crate::error::WikiError::io(&abs, e))?;

    Ok(LogEntry {
        date,
        op: op.to_string(),
        title: title.to_string(),
        header,
        body: detail.unwrap_or("").trim().to_string(),
    })
}

/// Return the most recent `n` log entries, newest last (document order).
pub fn log_recent(ws: &Workspace, n: usize) -> Result<Vec<LogEntry>> {
    let text = ws.read_file(LOG_REL).unwrap_or_default();
    let mut entries = parse_entries(&text);
    if entries.len() > n {
        entries = entries.split_off(entries.len() - n);
    }
    Ok(entries)
}

/// Parse all `## [..] .. | ..` entries out of a log document.
fn parse_entries(text: &str) -> Vec<LogEntry> {
    let lines: Vec<&str> = text.lines().collect();
    // Find indices of header lines.
    let mut headers: Vec<usize> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if is_entry_header(line) {
            headers.push(i);
        }
    }
    let mut out = Vec::with_capacity(headers.len());
    for (idx, &start) in headers.iter().enumerate() {
        let end = headers.get(idx + 1).copied().unwrap_or(lines.len());
        let header_line = lines[start];
        let body = lines[start + 1..end].join("\n").trim().to_string();
        let (date, op, title) = parse_header(header_line);
        out.push(LogEntry {
            date,
            op,
            title,
            header: header_line.to_string(),
            body,
        });
    }
    out
}

/// Is this a log entry header line (`## [` prefix)?
fn is_entry_header(line: &str) -> bool {
    line.trim_start().starts_with("## [")
}

/// Parse `## [YYYY-MM-DD] op | Title` into `(date, op, title)`.
fn parse_header(line: &str) -> (String, String, String) {
    let s = line.trim_start().trim_start_matches('#').trim();
    // Extract [date].
    let (date, rest) = if let Some(rest) = s.strip_prefix('[') {
        if let Some(end) = rest.find(']') {
            (rest[..end].to_string(), rest[end + 1..].trim().to_string())
        } else {
            (String::new(), s.to_string())
        }
    } else {
        (String::new(), s.to_string())
    };
    // Split op | title.
    let (op, title) = match rest.split_once('|') {
        Some((o, t)) => (o.trim().to_string(), t.trim().to_string()),
        None => (rest.trim().to_string(), String::new()),
    };
    (date, op, title)
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
    fn append_then_read_recent() {
        let (_d, ws) = init_ws();
        log_append(
            &ws,
            "ingest",
            "First Source",
            Some("touched 3 pages"),
            Some("2026-07-01"),
        )
        .unwrap();
        log_append(&ws, "query", "A question", None, Some("2026-07-02")).unwrap();
        log_append(&ws, "lint", "Health check", None, Some("2026-07-03")).unwrap();

        let recent = log_recent(&ws, 2).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].op, "query");
        assert_eq!(recent[1].op, "lint");
        assert_eq!(recent[1].title, "Health check");

        let all = log_recent(&ws, 100).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[0].date, "2026-07-01");
        assert_eq!(all[0].body, "touched 3 pages");
    }

    #[test]
    fn parses_header_variants() {
        assert_eq!(
            parse_header("## [2026-07-06] ingest | Article Title"),
            (
                "2026-07-06".to_string(),
                "ingest".to_string(),
                "Article Title".to_string()
            )
        );
    }
}
