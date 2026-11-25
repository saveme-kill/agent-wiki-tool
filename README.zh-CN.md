# agent-wiki

[![CI](https://github.com/saveme-kill/agent-wiki-tool/actions/workflows/ci.yml/badge.svg)](https://github.com/saveme-kill/agent-wiki-tool/actions/workflows/ci.yml)
![Rust: 1.85+](https://img.shields.io/badge/Rust-1.85%2B-orange.svg)

[English](./README.md) | 简体中文

**agent-wiki** 是一个由 **Agent 维护、持久化的 Markdown 知识库（wiki）**——[llm-wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f#file-llm-wiki-md)（[本地副本](./llm-wiki.md)）思想的 Rust 实现，同时提供 **`wiki` CLI** 和 **`wiki-mcp` MCP Server** 两种前端形态。

与传统 RAG（每次查询都从原始文档切片重新拼凑答案）不同，这里的 Agent 会**增量构建并持续保持最新**一个结构化、互相链接的知识库。交叉引用早已就位，矛盾之处早已标注，综合结论也已反映读过的全部内容。整个 wiki 是一个不断复利累积的产物，以纯 Markdown 文件形式存储于 Git 中。

人负责选材、探索、提问；Agent 负责所有记账与综合工作——摘要、交叉引用、归档、一致性维护。本项目为 Agent 提供完成这些工作的确定性**基础设施工具**：搜索、链接图、`index.md`/`log.md` 维护、`lint` 体检与上下文证据包。**Agent 负责写正文；工具只做导航与一致性维护。**

---

## 安装

### 下载预编译二进制

前往 [GitHub Releases](https://github.com/saveme-kill/agent-wiki-tool/releases) 下载对应平台（Linux `x86_64`、macOS `x86_64` / Apple Silicon `aarch64`、Windows `x86_64`）的压缩包，内含 `wiki` 与 `wiki-mcp`。

### 通过 Cargo 安装

需要 Rust `1.85+`：

```sh
cargo install --git https://github.com/saveme-kill/agent-wiki-tool wiki-cli wiki-mcp
```

### 从源码构建

```sh
git clone https://github.com/saveme-kill/agent-wiki-tool.git
cd agent-wiki-tool
cargo build --workspace --release
# 产出 target/release/wiki 和 target/release/wiki-mcp（Windows 下为 .exe）
```

---

## 工作区布局

`wiki init` 会脚手架出一个自包含的 Git 仓库，包含三层结构：

```text
workspace/
  AGENTS.md            # schema：约定 + Ingest/Query/Lint 工作流手册
  raw/                 # 不可变的原始来源文档（只读，source of truth）
  wiki/
    overview.md        # 顶层综述 / 演进中的 thesis
    index.md           # 目录：每页链接 + 一句话摘要，按类别组织
    log.md             # append-only 时序记录
    entities/  concepts/  sources/
  .wiki/index/         # tantivy 全文索引（.gitignore，可随时重建）
  .gitignore
```

页面使用 Obsidian 兼容的 YAML frontmatter（`type, title, tags, sources, origin, created, updated, status`）与 `[[wikilink]]` 交叉引用，因此整个知识库可直接用 Obsidian 打开（Graph View、Dataview 原生可用）。其中 `origin` 记录页面素材的上游出处 URL（如网页或飞书文档），是一等字段——`read` 会回显它，`lint` 会校验 `source` 页是否缺失出处。

---

## CLI 用法

```sh
wiki init ./my-wiki                                               # 脚手架 + git init
wiki -C ./my-wiki add-raw article.md                              # 注册一个原始来源
wiki -C ./my-wiki read-raw article.md                             # 分块读回来
wiki -C ./my-wiki write entities/ada \                            # 新建 / 覆盖页面
    --title "Ada Lovelace" --type entity \
    --body 'First programmer; see [[concepts/analytical-engine]].'
wiki -C ./my-wiki index                                           # 重建 index.md + 搜索索引
wiki -C ./my-wiki search "programmer"                             # BM25 搜索（中英文）
wiki -C ./my-wiki search "计算" --filter concepts                  # 按 scope/category 过滤
wiki -C ./my-wiki read concepts/analytical-engine                 # 正文 + frontmatter + 反向链接
wiki -C ./my-wiki backlinks entities/ada                          # 谁链接到这里
wiki -C ./my-wiki edit concepts/x "Features" --content "..."      # 局部 patch 某段
wiki -C ./my-wiki log --op ingest --title "Ada article"           # 追加一条 log
wiki -C ./my-wiki log --tail 5                                    # 显示最近的 log 条目
wiki -C ./my-wiki lint                                            # 结构体检
wiki -C ./my-wiki plan-ingest raw/article.md                      # 摄取前规划：标题、影响面、下一步
wiki -C ./my-wiki query-context "programmer"                      # 查询证据包：命中页、摘录、反链、raw 命中
wiki -C ./my-wiki review --diff                                   # 维护审阅：lint、log、git 变更、语义检查清单
```

任意命令加 `--json` 可得到与 MCP Server 完全一致的机器可读 JSON 输出。

---

## MCP Server

`wiki-mcp` 是 agent-wiki 的 MCP（Model Context Protocol）stdio server，将全部 14 个操作以工具形式暴露：
`wiki_init`、`wiki_search`、`wiki_read`、`wiki_write`、`wiki_edit`、`wiki_backlinks`、`wiki_index`、`wiki_log`、`wiki_lint`、`raw_add`、`raw_read`，以及高阶 Agent 工具 `wiki_plan_ingest`、`wiki_query_context`、`wiki_review`。工作区根目录取自 `WIKI_WORKSPACE` 环境变量（默认为当前目录）。

MCP 客户端配置示例：

```json
{
  "mcpServers": {
    "wiki": {
      "command": "/absolute/path/to/target/release/wiki-mcp",
      "env": {
        "WIKI_WORKSPACE": "/absolute/path/to/my-wiki"
      }
    }
  }
}
```

---

## 方法清单

CLI 与 MCP Server 都调用同一套 `wiki_core::ops`，行为完全一致：CLI 适合手工调试和脚本，MCP tool 适合 Agent 在对话中直接调用。

### 建库与来源层

| CLI | MCP tool | 主要参数 | 功能 | 典型使用时机 |
|---|---|---|---|---|
| `init [path]` | `wiki_init` | `path` | 初始化 wiki workspace：创建 `AGENTS.md`、`raw/`、`wiki/`、`.wiki/index/`、`.gitignore`，并 best-effort `git init` | 人或 Agent 第一次建知识库 |
| `add-raw <file>` | `raw_add` | CLI: `file`, `--name`, `--overwrite`; MCP: `name`, `content`, `overwrite` | 把原始来源注册到 `raw/`，默认不可覆盖，并增量写入搜索索引 | 新增本地 Markdown/文本来源；外部桥接脚本抓取后也走这里 |
| `read-raw <name>` | `raw_read` | `name`, `--max-chars` / `max_chars` | 按 Markdown heading 和字符预算把 raw source 切成 chunks 返回 | Agent 需要阅读长文但不能全量塞进上下文时 |
| `plan-ingest <raw>` | `wiki_plan_ingest` | `raw`, `--limit` / `limit` | 摄取前规划：提取 raw 标题/heading，建议 `sources/` 页面名，搜索可能受影响页面，返回语义检查清单 | Agent 写任何 source/entity/concept 页面之前 |

### 查询与导航

| CLI | MCP tool | 主要参数 | 功能 | 典型使用时机 |
|---|---|---|---|---|
| `query-context <query>` | `wiki_query_context` | `query`, `--limit`, `--max-pages`, `--max-body-chars` | 构建查询证据包：搜索命中、有限页面摘录、frontmatter、出链、反链、raw 命中和回写建议 | Agent 回答问题的首选入口，避免全量读 wiki/raw |
| `search <query>` | `wiki_search` | `query`, `--filter`, `--limit` | BM25 全文检索 wiki 页面和 raw 来源；支持 `wiki/raw/entities/concepts/sources/overview` 过滤 | 需要更窄搜索、补充查找或人工调试时 |
| `read <page>` | `wiki_read` | `page` | 读取页面正文、frontmatter、outbound `[[links]]` 和 backlinks | 证据包不足，需要精读某个页面时 |
| `backlinks <page>` | `wiki_backlinks` | `page` | 返回链接到目标页的页面列表 | 判断影响面、补双向链接、查概念/实体被哪些页面引用时 |

### 写入与维护

| CLI | MCP tool | 主要参数 | 功能 | 典型使用时机 |
|---|---|---|---|---|
| `write <page>` | `wiki_write` | `page`, `body`, `type/kind`, `title`, `tags`, `sources`, `origin`, `status` | 创建或覆盖 wiki 页面；自动推断/维护 frontmatter 日期，并增量索引 | 写 source 摘要、entity/concept 页面、overview 更新、沉淀查询结论 |
| `edit <page> <heading>` | `wiki_edit` | `page`, `heading`, `content`, `replace` | 在已存在 heading 下追加或替换段落，并增量索引；不会创建不存在的 heading | 对已有页面做小范围补充、修正、追加关联来源时 |
| `index` | `wiki_index` | 无 | 重建 `wiki/index.md` 目录和完整搜索索引 | 一批写入/编辑完成后的收尾校准 |
| `log` | `wiki_log` | 写入：`op`, `title`, `detail`; 读取：`tail` | 追加或读取 `wiki/log.md`，格式为 `## [YYYY-MM-DD] <op> \| <title>` | 记录 ingest/query/lint/review 等有意义动作；查询不会自动记 log |
| `lint` | `wiki_lint` | 无 | 结构体检：死链、孤儿页、stale、raw 缺 source page、source 缺 origin/sources、缺 title | 周期维护、写入后检查、提交前检查 |
| `review` | `wiki_review` | `--recent-log`, `--diff`, `--max-diff-chars` / `recent_log`, `include_diff`, `max_diff_chars` | 聚合 `lint`、最近 log、git changed files、可选 diff 摘录和语义审阅清单 | ingest/query 后交付前，或准备 commit 前 |

### 方法边界

- **Agent 负责判断和正文。** 工具只提供搜索、读取、写入、索引、链接图、lint/review 等确定性能力；跨文档综合、冲突判断、页面怎么写由 Agent 完成。
- **查询不会自动沉淀。** `search`、`read`、`query-context` 只读不写；有长期价值的结论需要 Agent 主动 `write/edit` 回 wiki，并 `log`。
- **`raw/` 是 source of truth。** `raw/` 默认不可覆盖；Agent 应读 raw、引用 raw，但不把分析结论写进 raw。
- **`write`/`edit`/`add-raw` 会增量索引。** `index` 用于一批操作完成后重建 `index.md` 和全量校准搜索索引。
- **`edit` 只 patch 已存在标题段。** 新页面或新结构先用 `write` 创建，再用 `edit` 做局部维护。
- **优先高阶工具。** Agent 查询优先 `wiki_query_context`，摄取优先 `wiki_plan_ingest`，交付前优先 `wiki_review`。

---

## 三大工作流与闭环

每个工作区的 `AGENTS.md`（Agent 的操作手册）中都完整写明：

- **Ingest（摄取）** — `raw_add` → `wiki_plan_ingest` → `raw_read` → 与用户确认要点 → `wiki_write` 写一篇 `sources/` 摘要页 → `wiki_write`/`wiki_edit` 更新相关 `entities/`、`concepts/` 页并在双向添加 `[[链接]]` → `wiki_index` → `wiki_log` → `wiki_review`。一条来源可能触及 10–15 个页面。
- **Query（查询）** — `wiki_query_context`（或 `wiki_search` / 读 `index.md`）→ 必要时 `wiki_read` 深入并跟随链接 → 带引用综合作答 → 有价值的结论回存为新页，让探索成果同样沉淀 → `wiki_log`。
- **Review/Lint（审阅/体检）** — `wiki_review` 聚合 `wiki_lint`、最近 log、git 变更和语义检查清单；`wiki_lint` 输出死链、孤儿页、过时页、无摘要页的原始来源、缺少出处的 `source` 页、缺少标题的页面 → Agent 逐项修复。

```text
Ingest: init → add-raw → plan-ingest → read-raw → write(sources/…)
                                                 → write/edit ×N(entities/, concepts/)
                                                 → index → log → review/lint

Query:  query-context → (可选) read/backlinks → write 沉淀新页 → log
```

---

## 外部在线文档接入（飞书 / Lark 等）

`wiki-core` 刻意保持零网络依赖，`raw/` 只接收本地 UTF-8 文件。在线文档抓取放在编排边界的薄桥接脚本中（详见 [`scripts/README.md`](./scripts/README.md)）：

```sh
# 单篇飞书文档抓取（前置：安装并登录 lark-cli，且本机有 jq 与编译好的 wiki）
scripts/feishu-ingest.sh https://your-org.feishu.cn/docx/XXXX --workspace ./my-wiki

# 按 Markdown 清单批量导入飞书文档树并生成 stub source 页
python3 scripts/import-lark-docs.py \
    --manifest lark-doc.md \
    --manifest-title "Architecture Docs" \
    --workspace ./my-wiki
```

---

## 架构

一个 Cargo workspace，包含三个 crate：

- **`wiki-core`** — 全部领域逻辑（`workspace`/`page`/`link`/`index`/`logbook`/`search`/`ingest`/`lint`）加上 `ops` 编排层。不依赖任何协议或网络框架；包含全部单测。
- **`wiki-cli`** — `wiki` 二进制（clap），对 `wiki_core::ops` 的薄封装。
- **`wiki-mcp`** — `wiki-mcp` 二进制（rmcp 2.1 stdio server），对同一套 `ops` 的薄封装。

搜索使用内嵌的 Tantivy 0.26 BM25，并注册字符 n-gram 分析器（`"cjk"`）以零外部词典依赖同时支持中英日韩全文检索。

---

## 开发与测试

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo doc --workspace --no-deps
```

详细的架构约束、模块职责与 Tantivy/MCP 并发注意事项请参阅 [`AGENTS.md`](./AGENTS.md)。
