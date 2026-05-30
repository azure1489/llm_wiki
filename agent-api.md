# LLM Wiki Agent API 接口文档

> 面向 AI Agent（如 hermes agent）接入的只读知识库 HTTP API。
> 用户与 Agent 对话，Agent 通过本 API 检索 M Square 产品/系列/品类/公司知识，
> 拿到**带中文描述的产品图片**与正文，用于生成**推广文 / 图生图 / 产品促销信息**。

- **版本**: API `/api/v1`（应用版本 0.4.16）
- **协议**: HTTP/JSON，UTF-8
- **性质**: 只读检索为主（搜索、读页面、列文件、图谱）；写操作仅 `rescan`
- **最近更新**: 新增 `GET /agent/page`（agent 友好的结构化页面端点，图片返回开箱即用的 URL）

---

## 1. 接入信息

| 项 | 值 |
|---|---|
| 公网入口（推荐） | `https://wiki.aworld.ltd` |
| 本机直连（同机调试） | `http://127.0.0.1:19828` |
| API 前缀 | `/api/v1`（`/health` 例外，无需前缀） |
| 鉴权 | **必须**，Bearer Token（见下） |
| 当前知识库项目 | M Square，`projectId = ddce884e-d0ee-40ec-98cb-2830b153f565`，亦可用别名 `current` |

公网入口经 nginx + frp 转发到应用，TLS 在服务端终止。Agent 只需把 `https://wiki.aworld.ltd` 当普通 HTTPS API 用即可。

### 1.1 鉴权

所有 `/api/v1/**` 端点都需要 Token，缺失或错误返回 `401`。支持三种传法（任选其一）：

```http
Authorization: Bearer <TOKEN>
```
```http
X-LLM-Wiki-Token: <TOKEN>
```
```
?token=<TOKEN>          # 作为 query 参数（便于浏览器/取图直链场景）
```

> Token 由知识库管理员提供，请勿硬编码进客户端仓库。`/health` 端点无需鉴权。

### 1.2 通用约定

- **Project ID**：可传 UUID、项目别名 `current`（指向当前激活项目），或项目绝对路径。Agent 推荐用 `current` 或上面的固定 UUID。
- **响应包络**：成功 `{ "ok": true, ... }`；失败 `{ "ok": false, "error": "<原因>" }`，并带相应 HTTP 状态码。
- **Content-Type**：POST 请求体为 `application/json`。
- **CORS**：已开启（`Access-Control-Allow-Origin: *`，允许 `GET, POST, OPTIONS`，允许头 `Content-Type, Authorization, X-LLM-Wiki-Token`），可从浏览器侧直接调用。
- **限流**：每 IP 窗口 **120 请求 / 秒**，超出返回 `429`（`/health` 与 `OPTIONS` 不计）。
- **路径范围**：通过 API 只能读取项目内的公开路径——`wiki/**`、`raw/sources/**`、`purpose.md`、`schema.md`。其余（如内部状态 `.llm-wiki/`）返回 `403`，路径穿越返回 `400/403`。

---

## 2. 推荐工作流（Agent 视角）

```
用户对话： "给 K141510 儿童多功能手提包写一段小红书推广文，并配图"
        │
        ▼
① POST /search        ← 用关键词/产品名/型号检索，拿到最相关页面的 path
        │  results[].path
        ▼
② GET  /agent/page    ← 用该 path 取结构化页面：正文 body + images[]
        │  page.body（产品分析全文）
        │  page.images[] = { url, description, path }
        ▼
③ 生成
   · 推广文  ← 用 body（卖点/材质/系列/年份）+ images[].description（图中细节）写文案
   · 图生图  ← 用 images[].url 直接作为图生图模型的输入图（开箱即用，无需自己编码）
   · 促销信息 ← 综合 body + 关联实体（page.wikilinks / page.related）
```

**关键点**：第 ② 步 `/agent/page` 返回的 `images[].url` 是**可直接 GET 的完整 HTTPS 链接**（服务端已做好编码）。
第 ① 步 `/search` 返回的 `images[].url` 是**原始相对路径，不可直接取图**——仅用于预览判断，真正取图请走 `/agent/page`（详见 §4）。

---

## 3. 端点详解

### 3.1 `GET /health` — 健康检查（免鉴权）

确认服务存活与鉴权状态。

```bash
curl https://wiki.aworld.ltd/health
```
```json
{
  "ok": true,
  "status": "running",
  "version": "0.4.16",
  "enabled": true,
  "authRequired": true,
  "authConfigured": true,
  "allowUnauthenticated": false,
  "tokenSource": "store"
}
```

---

### 3.2 `GET /api/v1/projects` — 项目列表

返回所有已知项目及当前激活项目。

```bash
curl https://wiki.aworld.ltd/api/v1/projects \
  -H "Authorization: Bearer $TOKEN"
```
```json
{
  "ok": true,
  "projects": [
    { "id": "ddce884e-d0ee-40ec-98cb-2830b153f565", "name": "msquare", "path": "/Volumes/syn1t/llm-wiki/msquare", "current": true }
  ],
  "currentProject": { "id": "ddce884e-d0ee-40ec-98cb-2830b153f565", "name": "msquare", "path": "...", "current": true }
}
```

字段：`id` / `name` / `path` / `current`（是否当前激活）。

---

### 3.3 `POST /api/v1/projects/{id}/search` — 语义/关键词检索 ⭐

知识库的主入口。按查询返回最相关的 wiki/source 页面。

**请求体**（`application/json`）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `query` | string | 是 | 查询词：产品名、型号、系列、品类、"公司简介"等 |
| `topK` | int | 否 | 返回条数，默认 10，范围 1–50 |
| `includeContent` | bool | 否 | 是否在结果里附带页面全文，默认 false |
| `queryEmbedding` | number[] | 否 | 客户端自带的查询向量（一般不用） |

```bash
curl -X POST https://wiki.aworld.ltd/api/v1/projects/current/search \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"query":"儿童系列收纳袋","topK":5,"includeContent":false}'
```

**响应**：

```json
{
  "ok": true,
  "projectId": "ddce884e-...",
  "mode": "keyword",
  "tokenHits": 394,
  "vectorHits": 0,
  "results": [
    {
      "path": "wiki/sources/4-data--46-m%20square...--16dwqa.md",
      "title": "M Square 2015年儿童系列产品档案：K141510儿童多功能手提包",
      "snippet": "...## Embedded Images ### Document ![这是一个M Square...",
      "titleMatch": true,
      "score": 69.0,
      "vectorScore": null,
      "images": [
        {
          "alt": "这是一个M Square品牌的儿童系列收纳袋，采用透明塑料袋密封包装。收纳袋主体为黄色网格材质，配有粉色拉链…",
          "url": "media/4-data--46-m%20square...--16dwqa/001-10280.jpg"
        }
      ],
      "content": null
    }
  ]
}
```

字段说明：
- `mode`：`keyword`（纯关键词）/ `hybrid`（关键词+向量，需服务端启用 embeddingConfig）。当前为 `keyword`。
- `tokenHits` / `vectorHits`：关键词命中数 / 向量命中数。
- `results[]`：
  - `path`：页面项目相对路径 → **传给 `/agent/page` 取详情**。
  - `title` / `snippet`：标题与摘要片段。
  - `titleMatch`：是否标题命中。
  - `score`：综合得分（越大越相关）；`vectorScore`：向量得分（hybrid 时有值）。
  - `images[]`：`{ alt, url }`——`alt` 是中文图片描述，`url` 是**原始相对路径，不可直接取图**（见 §4）。仅用于快速判断该页有无相关图。
  - `content`：`includeContent=true` 时为页面全文，否则 `null`。

> Agent 建议：先 `search` 拿 `path`，再 `agent/page` 取可用的图片 URL 与干净正文，**不要直接用 search 里的 `images.url` 取图**。

---

### 3.4 `GET /api/v1/projects/{id}/agent/page` — 结构化页面（Agent 友好）⭐⭐

把一个 wiki/source 页面解析成结构化 JSON：解析好的 frontmatter、干净正文、**开箱即用的图片 URL + 中文描述**、关联 wikilink。Agent 无需自己解析 markdown/YAML。

**Query 参数**：

| 参数 | 必填 | 说明 |
|---|---|---|
| `path` | 是 | 页面项目相对路径，取自 `/search` 的 `results[].path` |

```bash
curl -G https://wiki.aworld.ltd/api/v1/projects/current/agent/page \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "path=wiki/sources/4-data--46-m%20square...--1osh0p2.md"
```

> 注意：`path` 里若含 `%`（媒体页路径常见），用 `--data-urlencode`（或客户端等价 encode）传，让 `%` 被正确转义。

**响应**：

```json
{
  "ok": true,
  "projectId": "ddce884e-...",
  "page": {
    "path": "wiki/sources/4-data--...--1osh0p2.md",
    "type": "source",
    "title": "Source: data/.../l253208心心魔术贴/product.md",
    "tags": [],
    "related": [],
    "sources": ["data/.../l253208心心魔术贴/product.md"],
    "frontmatter": {
      "created": "2026-05-30", "updated": "2026-05-30",
      "type": "source", "title": "...", "tags": [], "related": [], "sources": ["..."]
    },
    "body": "# Source: ...\n\n### 关键实体\n- **心心魔术贴 (l253208)** — 产品。M Square 2025年推出…",
    "images": [
      {
        "url": "https://wiki.aworld.ltd/wiki/media/4-data--46-m%2520square%25E4%25BA%25A7.../001-%E5%BF%83%E5%BF%83_01.jpg",
        "description": "这是一张M Square品牌儿童系列产品的包装图…袋身印有黄色\"m square kids\"字样…",
        "path": "wiki/media/4-data--46-m%20square.../001-心心魔术贴_01.jpg"
      }
    ],
    "wikilinks": ["心心魔术贴 l253208", "心心IP", "m-square"]
  }
}
```

`page` 字段：
- `path`：回显请求路径。
- `type`：页面类型，见 §5。
- `title`：页面标题。
- `tags` / `related` / `sources`：取自 frontmatter，便于做关联检索（`related` 为关联实体名，`sources` 为源文件路径）。
- `frontmatter`：完整 frontmatter 原样透传。
- `body`：正文 markdown 全文（含 LLM 生成的产品分析、卖点、内联图片）。
- **`images[]`**：每项 `{ url, description, path }`：
  - `url`：**可直接 GET 的完整 HTTPS 链接**（服务端已逐段 percent-encode，直接当图生图/展示输入用）。
  - `description`：该图的**中文 Vision 描述**（逐字含图中文字、颜色、材质、款式）。
  - `path`：原始项目相对路径（调试/备查用）。
  - 已按图片去重；外链 `http(s)://` 原样返回。
- `wikilinks`：正文里的 `[[实体]]` 列表，可继续用 `/search` 或 `/agent/page` 跳转。

---

### 3.5 `GET /api/v1/projects/{id}/files` — 列出文件树

浏览知识库结构（产品/系列目录、wiki 页面等）。

**Query 参数**：

| 参数 | 默认 | 说明 |
|---|---|---|
| `root` | `wiki` | `wiki` / `sources`（=`raw/sources`）/ `all`（公开根集合） |
| `recursive` | `true` | 是否递归子目录 |
| `maxFiles` | `2000` | 节点总数上限（含目录），范围 1–10000；**超出返回 413** |

```bash
curl -G https://wiki.aworld.ltd/api/v1/projects/current/files \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "root=wiki" --data-urlencode "recursive=false"
```
```json
{
  "ok": true, "projectId": "ddce884e-...", "root": "wiki", "truncated": false,
  "files": [
    { "name": "entities", "path": "wiki/entities", "isDir": true,  "size": null, "children": null },
    { "name": "overview.md", "path": "wiki/overview.md", "isDir": false, "size": 14342, "children": null }
  ]
}
```

文件节点：`name` / `path` / `isDir` / `size`（目录为 `null`）/ `children`（递归时为子节点数组，否则 `null`）。隐藏文件与符号链接被跳过。

> `recursive=false` 时若顶层条目数已超过 `maxFiles` 会返回 413——浏览大目录请调大 `maxFiles` 或分目录请求。

---

### 3.6 `GET /api/v1/projects/{id}/files/content` — 读取原始文本文件

返回某个文本文件的原始内容（未解析）。多数场景用 `/agent/page` 更合适。

**Query 参数**：`path`（必填，公开文本路径）。

```bash
curl -G https://wiki.aworld.ltd/api/v1/projects/current/files/content \
  -H "Authorization: Bearer $TOKEN" --data-urlencode "path=wiki/overview.md"
```
```json
{ "ok": true, "projectId": "ddce884e-...", "path": "wiki/overview.md", "content": "# 项目概览\n\n..." }
```

限制：仅文本类文件；单文件 > 2MB 返回 `413`；非 UTF-8 返回 `415`。

---

### 3.7 `GET /api/v1/projects/{id}/graph` — 知识图谱

返回 wiki 实体间的关系图（节点+边），可用于"相关产品/系列"扩展。

**Query 参数**：

| 参数 | 默认 | 说明 |
|---|---|---|
| `q` | — | 按 id/label 子串过滤节点 |
| `nodeType` | — | 按类型过滤（`source`/`entity`/`concept`…） |
| `limit` | `200` | 节点上限，范围 1–1000 |

```bash
curl -G https://wiki.aworld.ltd/api/v1/projects/current/graph \
  -H "Authorization: Bearer $TOKEN" --data-urlencode "q=儿童" --data-urlencode "limit=50"
```
```json
{
  "ok": true, "projectId": "ddce884e-...",
  "nodes": [
    { "id": "4-data--...--16dwqa", "label": "M Square 2015年儿童系列…K141510儿童多功能手提包",
      "nodeType": "source", "path": "wiki/sources/...md", "linkCount": 10 }
  ],
  "edges": [ { "source": "<nodeId>", "target": "<nodeId>", "weight": 1.0 } ]
}
```

只保留两端节点都在结果集内的边。

---

### 3.8 `POST /api/v1/projects/{id}/sources/rescan` — 触发重扫（写操作）

让知识库重新扫描源目录、增量入库。一般由管理员或自动化触发，Agent 通常不需要。

```bash
curl -X POST https://wiki.aworld.ltd/api/v1/projects/current/sources/rescan \
  -H "Authorization: Bearer $TOKEN"
```
返回 `{ "ok": true, "projectId": "...", "result": { ... } }`。

---

### 3.9 `POST /api/v1/projects/{id}/chat` — （未实现）

当前返回 `501`。聊天/RAG 管线仍在 WebView 内，尚未在本 HTTP API 暴露。Agent 侧请自行用 `search` + `agent/page` 组装上下文，在你自己的 LLM 里生成。

---

## 4. 图片取用说明（重要）

知识库的图片落盘在 `wiki/media/<源路径slug>/<文件名>`，由 nginx 两个 location 对外提供：

| URL 前缀 | 指向 | 用途 |
|---|---|---|
| `https://wiki.aworld.ltd/wiki/media/...` | 项目 `wiki/media/`（入库抽取+识别后的图） | **Agent 取图主用** |
| `https://wiki.aworld.ltd/media/...` | 原始数据目录 `data/` | 原始素材直链（按数据相对路径） |

**两个端点的图片字段语义不同：**

| 来源 | 字段 | 是否可直接取图 |
|---|---|---|
| `GET /agent/page` | `images[].url` | ✅ 完整 HTTPS、已编码，**直接 GET** |
| `POST /search` | `images[].url` | ❌ 原始相对路径（如 `media/.../001.jpg`），仅预览参考 |

**为什么 `/agent/page` 的 url 里有 `%2520` 这种双重编码**：media 目录名落盘时本身就含字面 `%20`/`%E4..`（源路径被 URL 编码进了目录名）。要让 nginx 解码后精确命中文件，URL 必须把字面 `%` 再编码成 `%25`。这层编码已由**服务端**做完——Agent 拿到 `url` **原样 GET 即可**，不要再自行 encode/decode。

```bash
# 直接用 agent/page 返回的 url 取图（无需任何额外处理）
curl -o product.jpg "https://wiki.aworld.ltd/wiki/media/4-data--46-m%2520square.../001-%E5%BF%83%E5%BF%83_01.jpg"
# → 200 image/jpeg
```

---

## 5. 页面类型（`page.type` / `nodeType`）

`wiki/` 下生成页：

| type | 目录 | 含义 |
|---|---|---|
| `overview` | `wiki/overview.md` | 项目总览 |
| `source` | `wiki/sources/` | 单个源文件（产品 product.md 等）的分析页，**含产品图片** |
| `entity` | `wiki/entities/` | 实体页（具体产品、系列、品牌等） |
| `concept` | `wiki/concepts/` | 概念页（如"场景化产品策略"） |
| `comparison` | `wiki/comparisons/` | 对比页 |
| `synthesis` | `wiki/synthesis/` | 综合/主题页 |
| `query` | `wiki/queries/` | 检索问答页（图谱中已过滤） |

`raw/sources/` 下的锚点 md（产品资料原文）类型由文件名体现：`product.md` / `series.md` / `brand.md` / `case.md` / `campaign.md` / `material.md` / `catalog.md` / `brochure.md` / `guide.md` / `custom.md`。

---

## 6. 错误码

| 状态码 | 含义 |
|---|---|
| `200` | 成功 |
| `400` | 参数缺失/非法（缺 `path`、JSON 解析失败、`root` 非法、路径穿越） |
| `401` | 未鉴权 / Token 错误 |
| `403` | 路径不在公开范围内 |
| `404` | 项目不存在 / 文件不存在 |
| `413` | 文件 > 2MB，或文件列表超过 `maxFiles` |
| `415` | 非文本/非 UTF-8 文件 |
| `429` | 触发限流（>120 req/s） |
| `500` | 服务端内部错误 |
| `501` | 端点未实现（`/chat`） |
| `503` | API 在设置中被禁用 |

错误体统一为：`{ "ok": false, "error": "<原因>" }`。

---

## 7. 配额与限制

| 项 | 值 |
|---|---|
| 请求体上限 | 1 MB |
| 单文件读取上限 | 2 MB |
| `search` topK | 1–50（默认 10） |
| `files` maxFiles | 1–10000（默认 2000） |
| `graph` limit | 1–1000（默认 200） |
| 限流 | 120 请求 / 秒 / IP |

---

## 8. Hermes Agent 接入范式（示例）

### 8.1 生成产品推广文

```
1) POST /search   {"query":"K141510 儿童多功能手提包","topK":3}
2) 取 results[0].path
3) GET  /agent/page?path=<上一步 path>
4) 用以下内容喂给写作 LLM：
   - page.body            → 产品定位、系列、年份、材质、卖点
   - page.images[].description → 图中可见的款式/颜色/文字细节
   - page.images[].url    → 选 1–3 张配图
   产出：标题 + 正文 + 配图链接
```

### 8.2 图生图（以官方产品图为输入）

```
1) POST /search   {"query":"心心魔术贴"}  → path
2) GET  /agent/page?path=...              → images[]
3) 挑选 images[].url（按 description 判断哪张最合适）
4) 直接把该 url 作为图生图模型的 init image（无需下载/再编码）
```

### 8.3 按品类/系列做促销合集

```
1) POST /search   {"query":"轻旅行系列","topK":20}
2) 对每个 results[].path 调 /agent/page
3) 聚合各页 body + images，生成系列促销长文 / 多图素材包
4) 可选：GET /graph?q=轻旅行 扩展关联产品
```

---

## 9. 部署侧配置（管理员）

| 配置 | 来源 | 默认 | 说明 |
|---|---|---|---|
| API Token | `apiConfig.token`（app-state）或环境变量 `LLM_WIKI_API_TOKEN` | — | 鉴权令牌 |
| 公网图片域名 | 环境变量 `LLM_WIKI_PUBLIC_BASE_URL` 或 `apiConfig.publicBaseUrl` | `https://wiki.aworld.ltd` | 用于拼 `agent/page` 的 `images[].url` |
| 监听端口 | 固定 | `127.0.0.1:19828` | 仅本机，公网经 nginx+frp 转发 |

> 改了 `publicBaseUrl` 后，新生成的 `agent/page` 响应里的图片 URL 即刻生效（无需重建数据）。
