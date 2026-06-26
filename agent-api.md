# LLM Wiki Agent API 接入指南

> 面向 AI Agent 接入的**知识库 HTTP API** 任务向快速上手。
> 用户与 Agent 对话，Agent 通过本 API **检索**知识库中的实体/概念/来源页面（拿到带文字描述的图片与干净正文，
> 用于内容生成 / 图生图 / 资料汇总）。把内容**写进知识库**仅在与应用**同机**时可用（外网默认只读，见 §3）。
>
> 完整协议参考（所有端点、参数、错误码、Clip Server、环境变量）见 [`docs/API.md`](docs/API.md)。

- **版本**: API `/api/v1`（应用版本 0.5.1）
- **协议**: HTTP/JSON，UTF-8
- **性质**: **外网经反代的只有 API Server（19828），且只读检索**（搜索、读页面、列文件、图谱、Review）；**写入仅同机可用**（Clip 端点未对外反代，见 §3）
- **核心端点**: `POST /search`（找页面）+ `GET /agent/page`（取结构化页面）；写入见 §3

> **占位符约定**：示例中 `$BASE` 为服务基址（本机 `http://127.0.0.1:19828`，或你的反代地址），
> `$TOKEN` 为鉴权令牌，`$PUBLIC_BASE` 为公网图片基址（`LLM_WIKI_PUBLIC_BASE_URL`）。请按部署替换。

---

## 1. 接入信息

| 项 | 值 |
|---|---|
| 服务基址 | `$BASE`（本机 `http://127.0.0.1:19828`；公网经反代，如 `https://<your-wiki-domain>`） |
| API 前缀 | `/api/v1`（`/health` 例外，无需前缀） |
| 鉴权 | 默认**必须**，Bearer Token（见下；管理员可在设置中关闭鉴权，仅限可信内网） |
| 项目标识 | 传 UUID、绝对路径，或别名 `current`（指向当前激活项目，**推荐**） |
| 写入入口 | Clip Server `http://127.0.0.1:19827`（**仅本机、无鉴权、未对外反代** → 外网不可写，见 §3） |

公网入口一般经 nginx / frp 转发到本机回环端口，TLS 在服务端终止。Agent 只需把基址当普通 HTTPS API 用即可。

### 1.1 鉴权

所有 `/api/v1/**` 端点默认需要 Token，缺失或错误返回 `401`。支持三种传法（任选其一）：

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
> Clip Server（写入）**无鉴权**，靠"仅监听本机"保证安全（见 §3）。

### 1.2 通用约定

- **Project ID**：可传 UUID、项目别名 `current`（指向当前激活项目），或项目绝对路径。Agent 推荐用 `current`。
- **响应包络**：成功 `{ "ok": true, ... }`；失败 `{ "ok": false, "error": "<原因>" }`，并带相应 HTTP 状态码。
- **Content-Type**：POST 请求体为 `application/json`。
- **CORS**：已开启（`Access-Control-Allow-Origin: *`，允许 `GET, POST, OPTIONS`，允许头 `Content-Type, Authorization, X-LLM-Wiki-Token`），可从浏览器侧直接调用。
- **限流**：每窗口 **120 请求 / 秒**，超出返回 `429`（`/health` 与 `OPTIONS` 不计）。
- **路径范围**：通过 API 只能读取项目内的公开路径——`wiki/**`、`raw/sources/**`、`purpose.md`、`schema.md`。其余（如内部状态 `.llm-wiki/`）返回 `403`，路径穿越返回 `400/403`。

---

## 2. 推荐工作流（检索，Agent 视角）

```
用户对话： "给 <某实体> 写一段介绍文，并配图"
        │
        ▼
① POST /search        ← 用关键词/实体名/编号检索，拿到最相关页面的 path
        │  results[].path
        ▼
② GET  /agent/page    ← 用该 path 取结构化页面：正文 body + images[]
        │  page.body（页面全文）
        │  page.images[] = { url, description, path }
        ▼
③ 生成
   · 文案    ← 用 body（要点/属性/关联）+ images[].description（图中细节）写文案
   · 图生图  ← 用 images[].url 直接作为图生图模型的输入图（开箱即用，无需自己编码）
   · 资料汇总 ← 综合 body + 关联实体（page.wikilinks / page.related）
```

**关键点**：第 ② 步 `/agent/page` 返回的 `images[].url` 是**可直接 GET 的完整链接**（服务端已做好编码）。
第 ① 步 `/search` 返回的 `images[].url` 是**原始相对路径，不可直接取图**——仅用于预览判断，真正取图请走 `/agent/page`（详见 §5）。

---

## 3. 把内容保存进知识库（写入）

> ⚠️ **先确认能不能写**：写入能力**仅限与应用同机**。
> 对外网只反代了 **API Server（19828）**，而它**没有"提交新内容"的端点**——唯一的写端点 `sources/rescan` 只是重扫**已在磁盘上**的文件、并不接收内容，`/chat` 未实现。
> 真正负责写入的 **Clip Server（19827）无鉴权、只监听 `127.0.0.1`、未做反代**，外网访问不到。
> **结论：外网 Agent 目前只能检索（§2、§4），无法直接把内容写进知识库。** 要写入，须在应用同机执行，或由管理员开通写入通道（见 §3.4）。

知识库写入遵循 **"raw source → 自动入库 → 生成 wiki 页面"** 的方法论：
**你不直接手写 `wiki/**` 页面**（实体/概念/综合页由入库管线 LLM 生成），而是把新内容作为**来源**放进 `raw/sources/`，再由应用的两步管线（**分析 → 生成**）增量产出 wiki 页面。

```
（同机）Agent 产出/采集的内容
        │
        ▼  ── 同机方式 A：POST :19827/clip（推荐）
   写入 raw/sources/<slug>-<日期>.md  ──┐
        │  ── 同机方式 B：自己落盘 + rescan ─┤
        ▼                                ▼
   加入入库队列  ──►  应用入库管线（分析 → 生成）  ──►  wiki/ 下新增/更新页面
```

> ⚠️ 入库由**正在运行的应用进程**异步处理，提交后 wiki 页面不会立即出现，需等管线跑完。

### 3.1 同机方式 A：`POST /clip` — 提交一段内容（推荐，仅本机）⭐

经 **Clip Server**（`http://127.0.0.1:19827`，**无前缀、无鉴权、仅本机**）提交一段 Markdown，服务端会：
1. 在目标项目 `raw/sources/` 下生成 `<slug>-<YYYYMMDD>.md`（带 `type: clip` frontmatter，重名自动加序号）；
2. 加入待入库队列，应用前端轮询后**自动入库**（分析 → 生成 wiki 页）。

> ⚠️ **安全约束**：Clip Server 无鉴权且会写文件，因此**只监听 `127.0.0.1`、不对公网暴露**——只有与应用**同机**的 Agent 能用。远程 Agent 无法经此写入（设计如此）。

**请求体**（`application/json`）：

| 字段 | 必填 | 说明 |
|---|---|---|
| `content` | 是 | 正文 Markdown（空 → 400） |
| `title` | 否 | 标题，默认 `Untitled`，用于生成文件名 slug 与写入 frontmatter |
| `url` | 否 | 原始网址，写入 frontmatter（无来源网址可留空） |
| `projectPath` | 否 | 目标项目**绝对路径**；缺省回落到"当前项目"（先用 `POST /project` 设定，见 §3.2） |

```bash
curl -X POST http://127.0.0.1:19827/clip -H "Content-Type: application/json" -d '{
  "title": "<标题>",
  "url": "https://example.com/x",
  "content": "# <标题>\n\n这里是要保存进知识库的正文 Markdown…",
  "projectPath": "/path/to/project"
}'
```
```json
{ "ok": true, "path": "raw/sources/<标题slug>-20260626.md" }
```

返回的 `path` 是新建源文件的项目相对路径。之后可用 §2 的检索流程查到它入库后生成的 wiki 页。

### 3.2 设定/查询写入目标项目（可选，仅本机）

不在 `/clip` 里带 `projectPath` 时，会写入 Clip Server 记住的"当前项目"。可显式设定：

```bash
# 设当前项目（之后不带 projectPath 的 /clip 都写这里）
curl -X POST http://127.0.0.1:19827/project -H "Content-Type: application/json" \
  -d '{"path":"/path/to/project"}'        # → {"ok":true}

curl http://127.0.0.1:19827/project       # 查当前项目 → {"ok":true,"path":"..."}
curl http://127.0.0.1:19827/projects      # 列已知项目
```

> `GET /clips/pending` 会**取出并清空**待入库队列——这是应用前端的轮询接口，Agent 一般无需调用（调了会把待入库项"抢走"，可能干扰自动入库）。

### 3.3 同机方式 B：自己落盘 + 触发重扫（同机、有文件系统权限时）

若进程能直接访问项目目录，可把 `.md` / `.pdf` / `.docx` 等源文件**自己写入** `raw/sources/`，再调 API Server 触发增量入库：

```bash
curl -X POST $BASE/api/v1/projects/current/sources/rescan -H "Authorization: Bearer $TOKEN"
```
```json
{ "ok": true, "projectId": "<project-uuid>", "result": { /* 重扫结果统计 */ } }
```

适合**批量导入**既有资料。入库用 **SHA256 增量缓存**，未变化的源文件不会重复处理。

> 注意：`rescan` 端点在 API Server（外网可达），但它**只处理已落盘到 `raw/sources/` 的文件**——"把文件放进去"这一步仍需同机文件系统权限。外网只调 `rescan` 不会新增任何内容。

### 3.4 外网 Agent 怎么写入 / 开通外网写入（管理员）

**现状**：外网侧**没有内容提交端点**。外网 Agent 若需"贡献内容"，当前只能把内容交给一个**同机组件**代写（例如在服务器上跑一个自带鉴权的小转发服务，收到请求后在本机 `POST :19827/clip`）。

如确需让外网直接写入，由管理员二选一开通（均需自行评估风险）：

1. **给 Clip Server 前置带鉴权的反代**：它本身**无鉴权且会写文件**，裸暴露等于开放匿名写盘——必须在前面加 **鉴权 + 限频 + 项目白名单**，再经 socat/nginx 暴露。
2. **在 API Server 上实现带 Token 的写端点**：目前代码没有此端点（`/chat` 占位为 501），需开发后才能用。

在此之前，**外网侧请把 llm-wiki 当只读检索源用**。

### 3.5 入库须知

- **不要手写 `wiki/**` 页面**：那是入库管线生成的产物；你只提交**来源**到 `raw/sources/`。
- 入库是**两步异步**（分析 → 生成），由运行中的应用进程处理；提交≠立即可检索。
- 重复提交同一内容：方式 B 因 SHA256 去重不会重复入库；方式 A 每次都会新建一个带日期的源文件（重名加序号），注意避免重复堆积。

---

## 4. 端点详解（检索）

> 仅列 Agent 常用的检索端点。写入端点见 §3；完整端点（含 `files`、`graph`、`reviews`、Clip Server 全部）见 [`docs/API.md`](docs/API.md)。

### 4.1 `GET /health` — 健康检查（免鉴权）

确认服务存活与鉴权状态。

```bash
curl $BASE/health
```
```json
{
  "ok": true,
  "status": "running",
  "version": "0.5.1",
  "enabled": true,
  "authRequired": true,
  "authConfigured": true,
  "allowUnauthenticated": false,
  "tokenSource": "store"
}
```

---

### 4.2 `GET /api/v1/projects` — 项目列表

返回所有已知项目及当前激活项目。

```bash
curl $BASE/api/v1/projects -H "Authorization: Bearer $TOKEN"
```
```json
{
  "ok": true,
  "projects": [
    { "id": "<project-uuid>", "name": "<project-name>", "path": "/path/to/project", "current": true }
  ],
  "currentProject": { "id": "<project-uuid>", "name": "<project-name>", "path": "/path/to/project", "current": true }
}
```

字段：`id` / `name` / `path` / `current`（是否当前激活）。

---

### 4.3 `POST /api/v1/projects/{id}/search` — 语义/关键词检索 ⭐

知识库的主入口。按查询返回最相关的 wiki/source 页面。

**请求体**（`application/json`）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `query` | string | 是 | 查询词：实体名、编号、概念、关键词等 |
| `topK` | int | 否 | 返回条数，默认 10，范围 1–50 |
| `includeContent` | bool | 否 | 是否在结果里附带页面全文，默认 false |
| `queryEmbedding` | number[] | 否 | 客户端自带的查询向量（一般不用） |

```bash
curl -X POST $BASE/api/v1/projects/current/search \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"query":"<查询词>","topK":5,"includeContent":false}'
```

**响应**：

```json
{
  "ok": true,
  "projectId": "<project-uuid>",
  "mode": "keyword",
  "tokenHits": 42,
  "vectorHits": 0,
  "results": [
    {
      "path": "wiki/entities/<page-slug>.md",
      "title": "<页面标题>",
      "snippet": "...命中片段...",
      "titleMatch": true,
      "score": 69.0,
      "vectorScore": null,
      "images": [
        { "alt": "图片的文字描述…", "url": "media/<slug>/001-<file>.jpg" }
      ],
      "content": null
    }
  ]
}
```

字段说明：
- `mode`：`keyword`（纯关键词）/ `hybrid`（关键词+向量，需服务端启用 `embeddingConfig`）。
- `tokenHits` / `vectorHits`：关键词命中数 / 向量命中数。
- `results[]`：
  - `path`：页面项目相对路径 → **传给 `/agent/page` 取详情**。
  - `title` / `snippet`：标题与摘要片段。
  - `titleMatch`：是否标题命中。
  - `score`：综合得分（越大越相关）；`vectorScore`：向量得分（hybrid 时有值，否则字段省略）。
  - `images[]`：`{ alt, url }`——`alt` 是图片文字描述，`url` 是**原始相对路径，不可直接取图**（见 §5）。仅用于快速判断该页有无相关图。
  - `content`：`includeContent=true` 时为页面全文，否则 `null`。

> Agent 建议：先 `search` 拿 `path`，再 `agent/page` 取可用的图片 URL 与干净正文，**不要直接用 search 里的 `images.url` 取图**。

---

### 4.4 `GET /api/v1/projects/{id}/agent/page` — 结构化页面（Agent 友好）⭐⭐

把一个 wiki/source 页面解析成结构化 JSON：解析好的 frontmatter、干净正文、**开箱即用的图片 URL + 文字描述**、关联 wikilink。Agent 无需自己解析 markdown/YAML。

**Query 参数**：

| 参数 | 必填 | 说明 |
|---|---|---|
| `path` | 是 | 页面项目相对路径，取自 `/search` 的 `results[].path` |

```bash
curl -G $BASE/api/v1/projects/current/agent/page \
  -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "path=wiki/entities/<page-slug>.md"
```

> 注意：`path` 里若含 `%`（媒体页路径常见），用 `--data-urlencode`（或客户端等价 encode）传，让 `%` 被正确转义。

**响应**：

```json
{
  "ok": true,
  "projectId": "<project-uuid>",
  "page": {
    "path": "wiki/entities/<page-slug>.md",
    "type": "entity",
    "title": "<页面标题>",
    "tags": ["..."],
    "related": ["<关联实体>"],
    "sources": ["raw/sources/<源文件>.md"],
    "frontmatter": {
      "created": "2026-01-01", "updated": "2026-01-01",
      "type": "entity", "title": "...", "tags": [], "related": [], "sources": ["..."]
    },
    "body": "# <页面标题>\n\n正文 Markdown 全文…",
    "images": [
      {
        "url": "$PUBLIC_BASE/wiki/media/<slug>/001-<file>.jpg",
        "description": "图片的文字描述（逐字含图中文字、颜色、款式等细节）…",
        "path": "wiki/media/<slug>/001-<file>.jpg"
      }
    ],
    "wikilinks": ["<关联实体 A>", "<关联实体 B>"]
  }
}
```

`page` 字段：
- `path`：回显请求路径。
- `type`：页面类型，见 §6。
- `title`：页面标题。
- `tags` / `related` / `sources`：取自 frontmatter，便于做关联检索（`related` 为关联实体名，`sources` 为源文件路径）。
- `frontmatter`：完整 frontmatter 原样透传。
- `body`：正文 markdown 全文（含 LLM 生成的分析、要点、内联图片）。
- **`images[]`**：每项 `{ url, description, path }`：
  - `url`：**可直接 GET 的完整链接**（服务端已逐段 percent-encode，直接当图生图/展示输入用）。
  - `description`：该图的**文字描述**（逐字含图中文字、颜色、材质、款式）。
  - `path`：原始项目相对路径（调试/备查用）。
  - 已按图片去重；外链 `http(s)://` 原样返回。
- `wikilinks`：正文里的 `[[实体]]` 列表，可继续用 `/search` 或 `/agent/page` 跳转。

---

## 5. 图片取用说明（重要）

知识库的图片落盘在 `wiki/media/<源路径slug>/<文件名>`，由前置反向代理对外提供。

**两个端点的图片字段语义不同：**

| 来源 | 字段 | 是否可直接取图 |
|---|---|---|
| `GET /agent/page` | `images[].url` | ✅ 完整链接、已编码，**直接 GET** |
| `POST /search` | `images[].url` | ❌ 原始相对路径（如 `media/.../001.jpg`），仅预览参考 |

**为什么 `/agent/page` 的 url 里可能有 `%2520` 这种双重编码**：media 目录名落盘时本身可能含字面 `%20`/`%XX`（源路径被 URL 编码进了目录名）。要让反向代理解码后精确命中文件，URL 必须把字面 `%` 再编码成 `%25`。这层编码已由**服务端**做完——Agent 拿到 `url` **原样 GET 即可**，不要再自行 encode/decode。

```bash
# 直接用 agent/page 返回的 url 取图（无需任何额外处理）
curl -o image.jpg "$PUBLIC_BASE/wiki/media/<slug>/001-<file>.jpg"
# → 200 image/jpeg
```

> `$PUBLIC_BASE` 即管理员配置的 `LLM_WIKI_PUBLIC_BASE_URL`（公网图片基址）。

---

## 6. 页面类型（`page.type` / `nodeType`）

`wiki/` 下生成页：

| type | 目录 | 含义 |
|---|---|---|
| `overview` | `wiki/overview.md` | 项目总览 |
| `source` | `wiki/sources/` | 单个源文件的分析页，**含来源图片** |
| `entity` | `wiki/entities/` | 实体页 |
| `concept` | `wiki/concepts/` | 概念页 |
| `comparison` | `wiki/comparisons/` | 对比页 |
| `synthesis` | `wiki/synthesis/` | 综合/主题页 |
| `query` | `wiki/queries/` | 检索问答页（图谱中已过滤） |

`raw/sources/` 下的锚点 md（原始资料）类型由文件名体现，例如 `product.md` / `series.md` / `brand.md` / `case.md` / `campaign.md` / `material.md` / `catalog.md` / `brochure.md` / `guide.md` / `custom.md`（具体取决于项目的 `schema.md`）。经 `POST /clip` 写入的剪藏，frontmatter 为 `type: clip`。

---

## 7. 错误码

| 状态码 | 含义 |
|---|---|
| `200` | 成功 |
| `400` | 参数缺失/非法（缺 `path`/`content`、JSON 解析失败、`root` 非法、路径穿越） |
| `401` | 未鉴权 / Token 错误 |
| `403` | 路径不在公开范围内 |
| `404` | 项目不存在 / 文件不存在 / 路由未匹配 |
| `413` | 文件 > 2MB，或文件列表超过 `maxFiles` |
| `415` | 非文本/非 UTF-8 文件 |
| `429` | 触发限流（>120 req/s） |
| `500` | 服务端内部错误（含 `/clip` 写文件失败） |
| `501` | 端点未实现（`/chat`） |
| `503` | API 在设置中被禁用，或在途请求过多 |

错误体统一为：`{ "ok": false, "error": "<原因>" }`。

---

## 8. 配额与限制

| 项 | 值 |
|---|---|
| 请求体上限 | 1 MB |
| 单文件读取上限 | 2 MB |
| `search` topK | 1–50（默认 10） |
| `files` maxFiles | 1–10000（默认 2000） |
| `graph` limit | 1–1000（默认 200） |
| 限流 | 120 请求 / 秒（仅 API Server `/api/v1/**`） |

---

## 9. Agent 接入范式（示例）

### 9.1 生成介绍/推广文

```
1) POST /search   {"query":"<实体名/编号>","topK":3}
2) 取 results[0].path
3) GET  /agent/page?path=<上一步 path>
4) 用以下内容喂给写作 LLM：
   - page.body                 → 定位、属性、关联、要点
   - page.images[].description → 图中可见的款式/颜色/文字细节
   - page.images[].url         → 选 1–3 张配图
   产出：标题 + 正文 + 配图链接
```

### 9.2 图生图（以官方图片为输入）

```
1) POST /search   {"query":"<实体名>"}  → path
2) GET  /agent/page?path=...            → images[]
3) 挑选 images[].url（按 description 判断哪张最合适）
4) 直接把该 url 作为图生图模型的 init image（无需下载/再编码）
```

### 9.3 按主题做资料合集

```
1) POST /search   {"query":"<主题词>","topK":20}
2) 对每个 results[].path 调 /agent/page
3) 聚合各页 body + images，生成主题长文 / 多图素材包
4) 可选：GET /graph?q=<主题词> 扩展关联实体
```

### 9.4 把采集/生成的内容存入知识库（**仅同机** Agent；外网不可用，见 §3）

```
1) （首次，可选）POST :19827/project {"path":"/path/to/project"}   设定写入目标
2) POST :19827/clip {"title":"<标题>","url":"<可选>","content":"<Markdown 正文>"}
                                                  → {"ok":true,"path":"raw/sources/...md"}
3) 等应用入库管线跑完（分析 → 生成）
4) 之后即可用 9.1~9.3 的检索流程查到新生成的 wiki 页
   · 批量导入既有文件：自己写入 raw/sources/ 后调 POST /api/v1/projects/{id}/sources/rescan
```

> 外网 Agent 无法执行上面第 1~2 步（19827 未对外反代）；如需外网写入见 §3.4。

---

## 10. 部署侧配置（管理员）

| 配置 | 来源 | 默认 | 说明 |
|---|---|---|---|
| API Token | `apiConfig.token`（app-state）或环境变量 `LLM_WIKI_API_TOKEN` | — | 鉴权令牌 |
| 公网图片基址 | 环境变量 `LLM_WIKI_PUBLIC_BASE_URL` 或 `apiConfig.publicBaseUrl` | 构建内置占位域名，**部署应覆盖** | 用于拼 `agent/page` 的 `images[].url` |
| API 监听地址 | 环境变量 `LLM_WIKI_API_HOST` / `LLM_WIKI_API_PORT` | `127.0.0.1:19828` | 检索 API；默认仅本机，公网经反代/socat 转发 |
| Clip 监听地址 | 环境变量 `LLM_WIKI_CLIP_HOST` / `LLM_WIKI_CLIP_PORT` | `127.0.0.1:19827` | 写入入口（无鉴权）；**保持本机、勿对外暴露** |

> 改了 `publicBaseUrl` 后，新生成的 `agent/page` 响应里的图片 URL 即刻生效（无需重建数据）。
> 监听地址、多实例隔离、完整端点参考见 [`docs/API.md`](docs/API.md) 与 [`deploy/MULTI.md`](deploy/MULTI.md)。
