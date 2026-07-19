//! Full-text search over the wiki using tantivy (BM25).
//!
//! Per the llm-wiki note, at moderate scale the index file alone is often
//! enough — but proper search helps as the wiki grows. We use tantivy's
//! embedded BM25 with a small on-disk index under `.wiki/index/` (gitignored,
//! rebuildable from markdown at any time).
//!
//! ## CJK + English
//!
//! tantivy 0.26 ships no dedicated CJK tokenizer. We register an
//! [`NgramTokenizer`]`(1, 2)` analyzer (plus a lowercaser) under the name
//! `"cjk"`: it emits character uni/bi-grams over the whole string, so BM25
//! recall works for un-spaced Chinese/Japanese/Korean *and* ordinary English
//! tokens. This keeps us dependency-free (no jieba/lindera) at the cost of a
//! somewhat larger index — an acceptable trade at wiki scale.
//!
//! ## Schema
//!
//! - `path`     STRING|STORED — unique key (workspace-relative), used for upsert.
//! - `scope`    STRING|STORED — `wiki` or `raw`, for the coarse filter.
//! - `category` STRING|STORED — `entities`/`concepts`/`sources`/`overview`/`raw`/`other`.
//! - `title`    TEXT(cjk)|STORED — searched + shown in results.
//! - `body`     TEXT(cjk)|STORED — searched + used to build snippets.

use std::path::Path;

use serde::Serialize;
use tantivy::collector::TopDocs;
use tantivy::directory::MmapDirectory;
use tantivy::query::{BooleanQuery, Occur, Query, QueryParser, TermQuery};
use tantivy::schema::{
    Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, Value, STORED, STRING,
};
use tantivy::tokenizer::{LowerCaser, NgramTokenizer, RemoveLongFilter, TextAnalyzer};
use tantivy::{doc, Index, IndexWriter, ReloadPolicy, TantivyDocument, Term};

use crate::error::{Result, WikiError};
use crate::page::Page;
use crate::workspace::{page_stem, Workspace};

const CJK_TOKENIZER: &str = "cjk";
const WRITER_HEAP: usize = 50_000_000;

/// A single search result.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchHit {
    /// Workspace-relative path of the matching page or raw source.
    pub path: String,
    /// `wiki` or `raw`.
    pub scope: String,
    /// Fine-grained category (`entities`, `concepts`, `sources`, `raw`, …).
    pub category: String,
    pub title: String,
    /// BM25 relevance score.
    pub score: f32,
    /// A short excerpt around the match (best-effort, plain text).
    pub snippet: String,
}

/// An open handle to the tantivy index for a workspace.
pub struct SearchIndex {
    index: Index,
    path: Field,
    scope: Field,
    category: Field,
    title: Field,
    body: Field,
}

impl SearchIndex {
    /// Build the schema and return it alongside the field handles.
    fn schema() -> (Schema, Field, Field, Field, Field, Field) {
        let mut b = Schema::builder();
        let path = b.add_text_field("path", STRING | STORED);
        let scope = b.add_text_field("scope", STRING | STORED);
        let category = b.add_text_field("category", STRING | STORED);

        let cjk_indexing = TextFieldIndexing::default()
            .set_tokenizer(CJK_TOKENIZER)
            .set_index_option(IndexRecordOption::WithFreqsAndPositions);
        let cjk_stored = TextOptions::default()
            .set_indexing_options(cjk_indexing)
            .set_stored();

        let title = b.add_text_field("title", cjk_stored.clone());
        let body = b.add_text_field("body", cjk_stored);
        (b.build(), path, scope, category, title, body)
    }

    /// Register the "cjk" n-gram analyzer. Tokenizers live on the in-memory
    /// `Index` and are NOT persisted, so this must run after every open/create.
    fn register_tokenizer(index: &Index) {
        let ngram = NgramTokenizer::new(1, 2, false).expect("valid ngram params");
        let analyzer = TextAnalyzer::builder(ngram)
            .filter(RemoveLongFilter::limit(40))
            .filter(LowerCaser)
            .build();
        index.tokenizers().register(CJK_TOKENIZER, analyzer);
    }

    /// Open the workspace's index, creating it on disk if it doesn't exist.
    pub fn open(ws: &Workspace) -> Result<Self> {
        Self::open_at(&ws.index_dir())
    }

    fn open_at(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).map_err(|e| WikiError::io(dir, e))?;
        let (schema, path, scope, category, title, body) = Self::schema();
        let mmap = MmapDirectory::open(dir)?;
        let index = if Index::exists(&mmap)? {
            Index::open_in_dir(dir)?
        } else {
            Index::create_in_dir(dir, schema)?
        };
        Self::register_tokenizer(&index);
        Ok(SearchIndex {
            index,
            path,
            scope,
            category,
            title,
            body,
        })
    }

    /// Rebuild the entire index from the current wiki + raw files. Returns the
    /// number of documents indexed.
    pub fn reindex_all(ws: &Workspace) -> Result<usize> {
        // Open (or create) the index in place and clear it via the writer rather
        // than deleting the directory on disk. Removing the directory can race
        // with any `SearchIndex` still holding mmap handles (observed as an
        // "Directory not empty" error inside the long-lived MCP process), whereas
        // `delete_all_documents` + `commit` is safe and transactional.
        let si = Self::open_at(&ws.index_dir())?;
        let mut writer: IndexWriter = si.index.writer(WRITER_HEAP)?;
        writer.delete_all_documents()?;

        let mut count = 0usize;
        for rel in ws.list_pages()? {
            let raw = ws.read_file(&rel)?;
            let page = Page::parse(&raw, &rel)?;
            si.add_doc(
                &writer,
                &rel,
                "wiki",
                &wiki_category(&rel, &page),
                &page.effective_title(),
                &page.body,
            )?;
            count += 1;
        }
        for rel in ws.list_raw()? {
            let text = ws.read_file(&rel).unwrap_or_default();
            let title = page_stem(&rel).to_string();
            si.add_doc(&writer, &rel, "raw", "raw", &title, &text)?;
            count += 1;
        }
        writer.commit()?;
        Ok(count)
    }

    /// Incrementally upsert a single wiki page (delete-by-path then add + commit).
    pub fn upsert_page(&self, ws: &Workspace, rel: &str) -> Result<()> {
        let raw = ws.read_file(rel)?;
        let page = Page::parse(&raw, rel)?;
        let mut writer: IndexWriter = self.index.writer(WRITER_HEAP)?;
        writer.delete_term(Term::from_field_text(self.path, rel));
        self.add_doc(
            &writer,
            rel,
            "wiki",
            &wiki_category(rel, &page),
            &page.effective_title(),
            &page.body,
        )?;
        writer.commit()?;
        Ok(())
    }

    /// Incrementally upsert a single raw source.
    pub fn upsert_raw(&self, ws: &Workspace, rel: &str) -> Result<()> {
        let text = ws.read_file(rel).unwrap_or_default();
        let title = page_stem(rel).to_string();
        let mut writer: IndexWriter = self.index.writer(WRITER_HEAP)?;
        writer.delete_term(Term::from_field_text(self.path, rel));
        self.add_doc(&writer, rel, "raw", "raw", &title, &text)?;
        writer.commit()?;
        Ok(())
    }

    fn add_doc(
        &self,
        writer: &IndexWriter,
        path: &str,
        scope: &str,
        category: &str,
        title: &str,
        body: &str,
    ) -> Result<()> {
        writer.add_document(doc!(
            self.path => path,
            self.scope => scope,
            self.category => category,
            self.title => title,
            self.body => body,
        ))?;
        Ok(())
    }

    /// Search the index. `filter` narrows results by `scope` (`wiki`/`raw`) or
    /// `category` (`entities`/`concepts`/`sources`/`overview`). Returns hits
    /// sorted by descending BM25 score.
    pub fn search(
        &self,
        query_str: &str,
        filter: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SearchHit>> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        // Force the reader to observe the latest commit — the delay policy may
        // otherwise serve a stale segment set right after a same-process write.
        reader.reload()?;
        let searcher = reader.searcher();

        let mut parser = QueryParser::for_index(&self.index, vec![self.title, self.body]);
        // Weight title matches more heavily than body.
        parser.set_field_boost(self.title, 2.0);
        let user_query = parser
            .parse_query(query_str)
            .map_err(|e| WikiError::Search(format!("bad query {query_str:?}: {e}")))?;

        let query: Box<dyn Query> = match filter.map(|f| self.filter_term(f)) {
            Some(Some(term)) => {
                let filter_q = TermQuery::new(term, IndexRecordOption::Basic);
                Box::new(BooleanQuery::new(vec![
                    (Occur::Must, user_query),
                    (Occur::Must, Box::new(filter_q)),
                ]))
            }
            // Filter given but unrecognized -> treat as no filter.
            _ => user_query,
        };

        let top = searcher.search(&query, &TopDocs::with_limit(limit).order_by_score())?;
        let mut hits = Vec::with_capacity(top.len());
        for (score, addr) in top {
            let doc: TantivyDocument = searcher.doc(addr)?;
            let get = |f: Field| {
                doc.get_first(f)
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string()
            };
            let body = get(self.body);
            hits.push(SearchHit {
                path: get(self.path),
                scope: get(self.scope),
                category: get(self.category),
                title: get(self.title),
                score,
                snippet: make_snippet(&body, query_str),
            });
        }
        Ok(hits)
    }

    /// Total number of documents currently in the index (0 for a fresh index).
    pub fn num_docs(&self) -> Result<u64> {
        let reader = self
            .index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()?;
        reader.reload()?;
        Ok(reader.searcher().num_docs())
    }

    /// Map a filter string to a `Term` on either the `scope` or `category` field.
    fn filter_term(&self, filter: &str) -> Option<Term> {
        match filter.trim().to_lowercase().as_str() {
            "wiki" => Some(Term::from_field_text(self.scope, "wiki")),
            "raw" => Some(Term::from_field_text(self.scope, "raw")),
            "entities" | "entity" => Some(Term::from_field_text(self.category, "entities")),
            "concepts" | "concept" => Some(Term::from_field_text(self.category, "concepts")),
            "sources" | "source" => Some(Term::from_field_text(self.category, "sources")),
            "overview" => Some(Term::from_field_text(self.category, "overview")),
            _ => None,
        }
    }
}

/// Classify a wiki page into a search category.
fn wiki_category(rel: &str, page: &Page) -> String {
    match page.frontmatter.kind.as_str() {
        "entity" => return "entities".into(),
        "concept" => return "concepts".into(),
        "source" => return "sources".into(),
        "overview" => return "overview".into(),
        _ => {}
    }
    if rel.contains("/entities/") {
        "entities".into()
    } else if rel.contains("/concepts/") {
        "concepts".into()
    } else if rel.contains("/sources/") {
        "sources".into()
    } else if page_stem(rel) == "overview" {
        "overview".into()
    } else {
        "other".into()
    }
}

/// Build a short plain-text snippet: a window around the first occurrence of the
/// query (or any of its whitespace-separated tokens), else the body prefix.
fn make_snippet(body: &str, query: &str) -> String {
    const WINDOW: usize = 160;
    let body_flat: String = body.split_whitespace().collect::<Vec<_>>().join(" ");
    if body_flat.is_empty() {
        return String::new();
    }
    let hay = body_flat.to_lowercase();

    // Candidate needles: the whole query, then each token.
    let q = query.trim().to_lowercase();
    let mut needles = vec![q.clone()];
    needles.extend(q.split_whitespace().map(|s| s.to_string()));

    let mut hit_byte: Option<usize> = None;
    for needle in needles {
        if needle.len() < 2 {
            continue;
        }
        if let Some(pos) = hay.find(&needle) {
            hit_byte = Some(pos);
            break;
        }
    }

    let chars: Vec<char> = body_flat.chars().collect();
    let start_char = match hit_byte {
        Some(byte) => {
            // Convert byte offset in `hay` to a char index in `body_flat`.
            let char_idx = body_flat[..byte].chars().count();
            char_idx.saturating_sub(WINDOW / 4)
        }
        None => 0,
    };
    let end_char = (start_char + WINDOW).min(chars.len());
    let slice: String = chars[start_char..end_char].iter().collect();
    let mut out = String::new();
    if start_char > 0 {
        out.push('…');
    }
    out.push_str(slice.trim());
    if end_char < chars.len() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::page::Frontmatter;

    fn init_ws() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let (ws, _) = Workspace::init(dir.path()).unwrap();
        (dir, ws)
    }

    #[test]
    fn indexes_and_searches_english() {
        let (_d, ws) = init_ws();
        let mut fm = Frontmatter::default();
        fm.kind = "entity".into();
        fm.title = "Ada Lovelace".into();
        ws.write_page(
            "entities/ada",
            &Page::new(
                fm,
                "Ada was a mathematician who worked on the analytical engine.\n",
            ),
        )
        .unwrap();

        let n = SearchIndex::reindex_all(&ws).unwrap();
        assert!(n >= 1);
        let si = SearchIndex::open(&ws).unwrap();
        let hits = si.search("mathematician", None, 10).unwrap();
        assert!(hits.iter().any(|h| h.path == "wiki/entities/ada.md"));
        assert!(!hits[0].snippet.is_empty());
    }

    #[test]
    fn searches_cjk() {
        let (_d, ws) = init_ws();
        let mut fm = Frontmatter::default();
        fm.kind = "concept".into();
        fm.title = "分析机".into();
        ws.write_page(
            "concepts/analytical-engine",
            &Page::new(fm, "分析机是一种通用计算机器，由巴贝奇设计。\n"),
        )
        .unwrap();
        SearchIndex::reindex_all(&ws).unwrap();

        let si = SearchIndex::open(&ws).unwrap();
        let hits = si.search("计算", None, 10).unwrap();
        assert!(hits
            .iter()
            .any(|h| h.path == "wiki/concepts/analytical-engine.md"));
    }

    #[test]
    fn filter_by_scope_and_category() {
        let (_d, ws) = init_ws();
        crate::ingest::raw_add(&ws, "note.md", "shared keyword lives here", false).unwrap();
        let mut fm = Frontmatter::default();
        fm.kind = "concept".into();
        fm.title = "Keyword page".into();
        ws.write_page(
            "concepts/kw",
            &Page::new(fm, "shared keyword in a concept page.\n"),
        )
        .unwrap();
        SearchIndex::reindex_all(&ws).unwrap();

        let si = SearchIndex::open(&ws).unwrap();
        let raw_only = si.search("keyword", Some("raw"), 10).unwrap();
        assert!(raw_only.iter().all(|h| h.scope == "raw"));
        assert!(raw_only.iter().any(|h| h.path == "raw/note.md"));

        let concepts_only = si.search("keyword", Some("concepts"), 10).unwrap();
        assert!(concepts_only.iter().all(|h| h.category == "concepts"));
        assert!(concepts_only
            .iter()
            .any(|h| h.path == "wiki/concepts/kw.md"));
    }

    #[test]
    fn incremental_upsert_reflects_changes() {
        let (_d, ws) = init_ws();
        SearchIndex::reindex_all(&ws).unwrap();
        let si = SearchIndex::open(&ws).unwrap();

        let mut fm = Frontmatter::default();
        fm.title = "New".into();
        ws.write_page(
            "concepts/new",
            &Page::new(fm, "brandnewuniqueword appears.\n"),
        )
        .unwrap();
        si.upsert_page(&ws, "wiki/concepts/new.md").unwrap();

        let hits = si.search("brandnewuniqueword", None, 10).unwrap();
        assert!(hits.iter().any(|h| h.path == "wiki/concepts/new.md"));
    }

    #[test]
    fn reindex_is_safe_while_handle_is_open() {
        // Regression: in the long-lived MCP process, a `SearchIndex` handle stays
        // open across tool calls. A later full reindex must not error (previously
        // it removed the index dir and raced with the open mmap handles), and
        // searches must observe freshly-committed writes without a delay.
        let (_d, ws) = init_ws();
        let mut fm = Frontmatter::default();
        fm.title = "One".into();
        ws.write_page("concepts/one", &Page::new(fm, "alpha content.\n"))
            .unwrap();
        SearchIndex::reindex_all(&ws).unwrap();

        let si = SearchIndex::open(&ws).unwrap();
        assert!(si.num_docs().unwrap() >= 1);

        // Add another page and run a full reindex while `si` is still alive.
        let mut fm2 = Frontmatter::default();
        fm2.title = "Two".into();
        ws.write_page("concepts/two", &Page::new(fm2, "betacontentword here.\n"))
            .unwrap();
        SearchIndex::reindex_all(&ws).unwrap(); // must not error

        // A search on the still-open handle must see the new document.
        let hits = si.search("betacontentword", None, 10).unwrap();
        assert!(hits.iter().any(|h| h.path == "wiki/concepts/two.md"));
    }
}
