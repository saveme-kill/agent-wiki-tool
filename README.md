# agent-wiki

**agent-wiki** 是一个由 **Agent 维护、持久化的 markdown 知识库（wiki）**——[llm-wiki](https://gist.github.com/karpathy/442a6bf555914893e9891c11519de94f#file-llm-wiki-md)
思想的实现，同时提供 **`wiki` CLI** 和 **`wiki-mcp` MCP server** 两种形态。

与传统 RAG（每次查询都从原始文档重新拼凑答案）不同，这里的 Agent 会**增量构建并持续
保持最新**一个结构化、互相链接的知识库。交叉引用早已就位，矛盾之处早已标注，综合结论也
已反映读过的全部内容。整个 wiki 是一个不断累积的产物，以纯 markdown 文件形式存储于 git 中。

人负责选材、探索、提问；Agent 负责所有记账工作——摘要、交叉引用、归档、一致性维护。本项目
为 Agent 提供完成这些工作的确定性**工具**：搜索、链接图、index/log 维护、lint 检查。
**Agent 负责写正文；工具只做导航与一致性维护。**

## 工作区布局

`wiki init` 会脚手架出一个 git 仓库，包含三层结构：

```
workspace/
  AGENTS.md            # schema：约定 + Ingest/Query/Lint 工作流
  raw/                 # 不可变的原始来源文档（只读，source of truth）
  wiki/
    overview.md        # 顶层综述 / 演进中的 thesis
    index.md           # 目录：每页链接 + 一句话摘要，按类别组织
    log.md             # append-only 时序记录
    entities/  concepts/  sources/
  .wiki/index/         # tantivy 全文索引（.gitignore，可随时重建）
  .gitignore
```

页面使用 Obsidian 兼容的 YAML frontmatter（`type, title, tags, sources,
origin, created, updated, status`）与 `[[wikilink]]` 交叉引用，因此 wiki 也能直接在
Obsidian 中使用（graph view、Dataview）。其中 `origin` 记录页面素材的上游出处
URL（如一篇飞书文档），是一等字段——`read` 会回显它，`lint` 会校验 source 页是否缺失出处。

## 构建

```sh
cargo build --workspace --release
# 产出 target/release/wiki  和  target/release/wiki-mcp
```

## CLI 用法

```sh
wiki init ./my-wiki                 # 脚手架 + git init
wiki -C ./my-wiki add-raw article.md            # 注册一个原始来源
wiki -C ./my-wiki read-raw article.md           # 分块读回来
wiki -C ./my-wiki write entities/ada \          # 新建 / 覆盖页面
    --title "Ada Lovelace" --type entity \
    --body 'First programmer; see [[concepts/analytical-engine]].'
wiki -C ./my-wiki index                          # 重建 index.md + 搜索索引
wiki -C ./my-wiki search "programmer"            # BM25 搜索（中英文）
wiki -C ./my-wiki search "计算" --filter concepts # 按 scope/category 过滤
wiki -C ./my-wiki read concepts/analytical-engine   # 正文 + frontmatter + 反向链接
wiki -C ./my-wiki backlinks entities/ada         # 谁链接到这里
wiki -C ./my-wiki edit concepts/x "Features" --content "..."   # 局部 patch 某段
wiki -C ./my-wiki log --op ingest --title "Ada article"        # 追加一条 log
wiki -C ./my-wiki log --tail 5                   # 显示最近的 log 条目
wiki -C ./my-wiki lint                           # 结构体检
wiki -C ./my-wiki plan-ingest raw/article.md     # 摄取前规划：标题、影响面、下一步
wiki -C ./my-wiki query-context "programmer"     # 查询证据包：命中页、摘录、反链、raw 命中
wiki -C ./my-wiki review --diff                  # 维护审阅：lint、log、git 变更、语义检查清单
```

任意命令加 `--json` 可得到机器可读的输出。

## MCP server

`wiki-mcp` 是 agent-wiki 的 MCP（Model Context Protocol）stdio server，将上述操作以工具形式暴露：
`wiki_init`、`wiki_search`、`wiki_read`、`wiki_write`、`wiki_edit`、`wiki_backlinks`、
`wiki_index`、`wiki_log`、`wiki_lint`、`raw_add`、`raw_read`，以及高阶 Agent 工具
`wiki_plan_ingest`、`wiki_query_context`、`wiki_review`。工作区根目录取自
`WIKI_WORKSPACE` 环境变量（默认为当前目录）。

MCP 客户端配置示例：

```json
{
  "mcpServers": {
    "wiki": {
      "command": "/absolute/path/to/target/release/wiki-mcp",
      "env": { "WIKI_WORKSPACE": "/absolute/path/to/my-wiki" }
    }
  }
}
```

## 方法清单

CLI 与 MCP server 都调用同一套 `wiki_core::ops`，所以行为一致：CLI 适合手工调试和脚本，
MCP tool 适合 Agent 在对话中直接调用。Agent 使用时优先请求 JSON 输出或直接走 MCP tool。

### 建库与来源层

| CLI | MCP tool | 主要参数 | 功能 | 典型使用时机 |
|---|---|---|---|---|
| `init [path]` | `wiki_init` | `path` | 初始化 wiki workspace：创建 `AGENTS.md`、`raw/`、`wiki/`、`.wiki/index/`、`.gitignore`，并 best-effort `git init` | 人或 Agent 第一次建知识库 |
| `add-raw <file>` | `raw_add` | CLI: `file`, `--name`, `--overwrite`; MCP: `name`, `content`, `overwrite` | 把原始来源注册到 `raw/`，默认不可覆盖，并增量写入搜索索引 | 新增本地 Markdown/文本来源；飞书脚本抓取后也走这里 |
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
- **raw 是 source of truth。** `raw/` 默认不可覆盖；Agent 应读 raw、引用 raw，但不把分析结论写进 raw。
- **`write`/`edit`/`add-raw` 会增量索引。** `index` 用于一批操作完成后重建 `index.md` 和全量校准搜索索引。
- **`edit` 只 patch 已存在标题段。** 新页面或新结构先用 `write` 创建，再用 `edit` 做局部维护。
- **优先高阶工具。** Agent 查询优先 `wiki_query_context`，摄取优先 `wiki_plan_ingest`，交付前优先 `wiki_review`。

## 三大工作流

每个工作区的 `AGENTS.md`（Agent 的操作手册）中都完整写明：

- **Ingest（摄取）** — `raw_add` → `wiki_plan_ingest` → `raw_read` → 与用户确认要点 → `wiki_write` 写一篇
  source 摘要页 → `wiki_write`/`wiki_edit` 更新相关 entity/concept 页并在双向添加
  `[[链接]]` → `wiki_index` → `wiki_log` → `wiki_review`。一条来源可能触及 10–15 个页面。
- **Query（查询）** — `wiki_query_context`（或 `wiki_search` / 读 `index.md`）→
  必要时 `wiki_read` 深入并跟随链接 →
  带引用综合作答 → 有价值的结论回存为新页，让探索成果同样沉淀 → `wiki_log`。
- **Review/Lint（审阅/体检）** — `wiki_review` 聚合 `wiki_lint`、最近 log、git 变更和语义检查清单；
  `wiki_lint` 输出结构问题：死链、孤儿页、过时页、无摘要页的原始来源、
  缺少出处的 source 页（`origin`/`sources` 皆空）、缺少标题的页面 → Agent 逐项修复。

## 端到端构建流程

上面三大工作流的接口级展开：每一步对应的 CLI 命令与 MCP 工具（两个前端底层
调用同一套 `wiki_core::ops`，行为完全一致；Agent 通常加 `--json` 解析结构化输出）。

### 接口映射

| 阶段 | CLI 命令 | MCP 工具 | 触发者 |
|---|---|---|---|
| 建库 | `init` | `wiki_init` | 人（一次） |
| 摄取 ① 注册来源 | `add-raw` | `raw_add` | 人 / Agent |
| 摄取 ② 规划影响面 | `plan-ingest` | `wiki_plan_ingest` | Agent |
| 摄取 ③ 分块读回 | `read-raw` | `raw_read` | Agent |
| 摄取 ④ 写页面 | `write` | `wiki_write` | Agent |
| 摄取 ⑤ 局部补丁 | `edit` | `wiki_edit` | Agent |
| 摄取 ⑥ 刷新目录 | `index` | `wiki_index` | Agent |
| 摄取 ⑦ 记账 | `log` | `wiki_log` | Agent |
| 查询 ① 证据包 | `query-context` | `wiki_query_context` | Agent |
| 查询 ② 检索 | `search` | `wiki_search` | Agent |
| 查询 ③ 精读 | `read` | `wiki_read` | Agent |
| 查询 ④ 查引用 | `backlinks` | `wiki_backlinks` | Agent |
| 审阅/体检 | `review` / `lint` | `wiki_review` / `wiki_lint` | Agent |

### 一次 Ingest 的调用顺序

```
init → add-raw → plan-ingest → read-raw → write(sources/… 摘要页)
                              → write ×N(entities/、concepts/，正文双向加 [[链接]])
                                  └─（可选）edit 追加/替换已有段
                              → index → log → review/lint
```

一条来源可能触及 10–15 个页面；这种广泛的交叉引用正是重点。

### 三个关键行为

- **写操作自动增量索引。** `write` / `edit` / `add-raw` 的返回里 `reindexed: true`，
  表示已把该页 / 来源写进搜索索引。因此 `index` **不必每页调用**——它用于批量写完后
  **一次性重建 `index.md` 目录 + 全量校准**，是 Ingest 的收尾步。
- **`edit` 只 patch 已存在的标题段，不创建标题。** 对不存在的 heading 调用会报
  `heading '…' not found`。段落的首次内容用 `write` 建好（正文含 `## 标题`），`edit`
  只用于往已有段追加或替换。
- **`search` 惰性建索引。** 全新工作区第一次 `search` 会自动全量索引，无需先手动 `index`。

### Query 闭环（让探索也沉淀）

```
query-context(命中 + page 摘录 + raw 命中 + backlinks) → 必要时 read/backlinks 深挖
       → 有价值的综合结论 write 成新页 → log 一条 query
```

`wiki_read` 一次返回正文、frontmatter、出链和反向链接——关系图早已建好，直接读即可，
无需像传统 RAG 那样每次重新拼装。

### 高阶 Agent 工具

- **`wiki_plan_ingest` / `plan-ingest`**：读取 raw 标题和 heading，生成建议的
  `sources/` 页面名，基于标题/heading 搜索可能受影响页面，并返回“检查矛盾、补双向链接、更新
  overview”等语义检查清单。它不替 Agent 写正文，而是让 Agent 写之前知道影响面。
- **`wiki_query_context` / `query-context`**：一次返回有限数量的搜索命中、页面摘录、
  frontmatter、出链、反链和 raw 命中，避免查询时全量读 wiki/raw。
- **`wiki_review` / `review`**：聚合 `wiki_lint`、最近 `log.md`、git changed files、可选
  diff 摘录和语义审阅清单，用作 ingest/query 后提交前的健康检查。

## 飞书文档接入

日常来源常常是**飞书在线文档**，而 `raw/`（source of truth）刻意只吃本地 UTF-8 文件——
`wiki-core` 全程零网络依赖。因此抓取放在**编排边界的一层薄 bridge 脚本**里：飞书 URL →
（`lark-cli` 抓成 Markdown）→ `raw/feishu/*.md` → 走现有 ingest 流程。Rust 核心库不引入任何
HTTP 依赖。

前置：本机已装 `lark-cli` 并 `lark-cli auth login` 登录一次，且
`jq` 可用、`wiki` 已构建（`cargo build --release`）。

```sh
# 抓取一篇飞书文档到 raw/feishu/ 下（raw 文件名默认取飞书文档标题，可用 --name 覆盖）
scripts/feishu-ingest.sh https://your-org.feishu.cn/docx/XXXX \
    --workspace ./my-wiki
```

脚本会：① 用 `lark-cli docs +fetch --api-version v2 --doc <URL> --doc-format markdown`
抓正文（失败会提示先 `lark-cli auth login`）；② 在文件头写入 `<!-- origin: <URL> -->`
与 `<!-- fetched: <date> -->` 注释（不污染正文、可 grep）；③ 调 `wiki add-raw` 落到
`raw/feishu/<飞书文档标题>.md`（默认不可覆盖；标题不可用时回退 URL token）；④ 打印下一步提示，引导 Agent `read-raw` →
`write` 一篇以飞书文档标题命名的 `sources/` 摘要页，并把出处填进 frontmatter：

```sh
wiki -C ./my-wiki write "sources/Ada 传记" --type source \
    --title "Ada 传记" \
    --origin https://your-org.feishu.cn/docx/XXXX \
    --source "raw/feishu/Ada 传记.md" \
    --body '摘要正文，含 [[entities/ada-lovelace]] 等 [[wikilinks]]。'
```

`origin` 作为一等 frontmatter 字段：`wiki read "sources/Ada 传记"` 会回显它；对 `type: source`
且 `origin` 与 `sources` 均为空的页面，`wiki lint` 会报一条 `missing-origin`，提醒补齐出处。

## 架构

一个 Cargo workspace，包含三个 crate：

- **`wiki-core`** — 全部领域逻辑（workspace/page/link/index/log/search/ingest/
  lint）加上 `ops` 编排层。不依赖任何协议或 IO 框架；有完整单测。
- **`wiki-cli`** — `wiki` 二进制（clap），对 `wiki-core::ops` 的薄封装。
- **`wiki-mcp`** — `wiki-mcp` 二进制（rmcp stdio server），对同一套 `ops` 的薄封装。

把编排逻辑集中在 `wiki-core`，正是 CLI 与 MCP server 行为完全一致的保证。

搜索使用内嵌的 tantivy BM25。tantivy 0.26 未内置 CJK 分词器，因此用一个字符 n-gram
分析器（注册名 `"cjk"`）为无空格的中日韩文本提供召回，同时兼容普通英文——无需外部分词器。

## 测试

```sh
cargo test --workspace
```
