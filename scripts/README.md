# External Source Bridges (`scripts/`)

`wiki-core` is intentionally **100% offline and network-free**: `wiki add-raw` (`raw_add`) only accepts local UTF-8 files so the core engine stays deterministic, fast, and credential-free.

To ingest authenticated online content (such as Feishu / Lark docs), fetch and convert documents to Markdown at the boundary and register them into `raw/` via `wiki add-raw`.

---

## 1. `feishu-ingest.sh` — Single Feishu Document

Fetches one Feishu/Lark online document as Markdown via `lark-cli`, prepends provenance HTML comments (`<!-- origin: ... -->` and `<!-- fetched: YYYY-MM-DD -->`), and registers it under `raw/feishu/<title>.md`.

### Prerequisites

- `lark-cli` installed and authenticated (`lark-cli auth login`)
- `jq` installed on `PATH`
- `wiki` built (`cargo build --release`) or installed on `PATH` (override via `WIKI_BIN=/path/to/wiki`)

### Usage

```sh
scripts/feishu-ingest.sh https://your-org.feishu.cn/docx/XXXX --workspace ./my-wiki

# Optional explicit slug under raw/feishu/<slug>.md
scripts/feishu-ingest.sh https://your-org.feishu.cn/docx/XXXX \
    --workspace ./my-wiki \
    --name product-spec
```

After registering the raw source, ask your agent to run `plan-ingest` → `read-raw` → `write` to synthesize the `sources/` summary page and update affected `entities/` and `concepts/` pages.

---

## 2. `import-lark-docs.py` — Bulk Manifest Importer

Parses a Markdown table of Feishu wiki documents (preserving hierarchy levels), fetches each document via `lark-cli`, stores it under `raw/feishu/`, and creates `status: stub` source pages under `wiki/sources/` with parent/child `[[wikilinks]]`.

### Expected Manifest Format

A Markdown file containing rows with ordinal, hierarchy depth level, title, backticked token, and link:

```markdown
| # | Level | Title | Token | Link |
|---:|---:|---|---|---|
| 1 | 1 | System Overview | `doxcnAbc123` | [Open](https://your-org.feishu.cn/docx/doxcnAbc123) |
| 2 | 2 | Storage Engine  | `doxcnDef456` | [Open](https://your-org.feishu.cn/docx/doxcnDef456) |
```

### Usage

```sh
python3 scripts/import-lark-docs.py \
    --manifest ./manifest.md \
    --manifest-title "Engineering Wiki Export" \
    --workspace ./my-wiki \
    --tag engineering
```

Run `python3 scripts/import-lark-docs.py --help` to see all options (`--manifest-page`, `--wiki-bin`, `--overwrite-raw`, `--limit`).

---

## Writing Your Own Bridge

Any external source (Notion, Confluence, GitHub Issues, web articles, PDFs converted via `markitdown`/`pandoc`) can follow the same three-step pattern:

1. Export the upstream document to UTF-8 Markdown.
2. Stamp `<!-- origin: <URL> -->` and `<!-- fetched: <YYYY-MM-DD> -->` at the top of the file.
3. Call `wiki -C <workspace> add-raw <temp_file> --name <subdir>/<slug>.md --json`, then hand control to the agent (`wiki_plan_ingest`) with `--origin <URL>` when creating the `sources/` page.
