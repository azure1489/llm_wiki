# LLM Wiki 本地 HTTP API — 完整接口参考

> 本文档是 LLM Wiki 桌面应用**对外 HTTP API 的完整协议参考**，与源码逐字对齐。
> 涵盖两个本地服务：**API Server**（知识库只读检索）与 **Clip Server**（网页剪藏写入）。
>
> - 实现源：`src-tauri/src/api_server.rs`（API Server）、`src-tauri/src/clip_server.rs`（Clip Server）
> - 应用版本：**0.5.1**　|　API 前缀：`/api/v1`　|　协议：HTTP/JSON，UTF-8
> - 多实例部署见 [`../deploy/MULTI.md`](../deploy/MULTI.md)。

> **占位符约定**：下文示例中 `$BASE` 为服务基址（本机 `http://127.0.0.1:19828`，或你的反代地址），
> `$TOKEN` 为鉴权令牌，`{id}` 为项目标识（UUID / 绝对路径 / 别名 `current`）。请按自身部署替换。

---

## 目录

1. [两个本地服务总览](#1-两个本地服务总览)
2. [监听与部署配置（环境变量）](#2-监听与部署配置环境变量)
3. [鉴权模型](#3-鉴权模型)
4. [通用约定](#4-通用约定)
5. [API Server 端点详解](#5-api-server-端点详解)
6. [Clip Server 端点详解](#6-clip-server-端点详解)
7. [数据模型与页面类型](#7-数据模型与页面类型)
8. [图片取用说明](#8-图片取用说明)
9. [错误码](#9-错误码)
10. [配额与限制](#10-配额与限制)
11. [对外暴露与多实例](#11-对外暴露与多实例)
12. [MCP / 客户端 SDK 对应关系](#12-mcp--客户端-sdk-对应关系)

---

## 1. 两个本地服务总览

应用在本机启动两个独立的 HTTP 服务，默认都只监听 `127.0.0.1`（回环），通过反向代理 / socat 对外暴露。

| 服务 | 默认监听 | 前缀 | 鉴权 | 职责 |
|---|---|---|---|---|
| **API Server** | `127.0.0.1:19828` | `/api/v1`（`/health` 例外） | **Token（可关）** | 项目/文件/页面/搜索/图谱/Review 只读检索 + `rescan` 写 |
| **Clip Server** | `127.0.0.1:19827` | 无（根路径） | **无鉴权** | 接收浏览器扩展剪藏，写入 `raw/sources/`；维护当前项目状态 |

> ⚠️ **Clip Server 无任何鉴权且会向项目目录写文件**，因此必须保持在 `127.0.0.1`，**切勿**经反向代理对公网暴露。对外只暴露 API Server。

两个服务都内置：绑定失败重试（3 次，每次间隔 2s）、CORS 全开、崩溃自恢复。

---

## 2. 监听与部署配置（环境变量）

所有监听地址都可经环境变量覆盖，便于在同一台主机上跑多个互相隔离的实例。

| 环境变量 | 作用 | 默认 |
|---|---|---|
| `LLM_WIKI_API_HOST` | API Server 绑定主机 | `127.0.0.1` |
| `LLM_WIKI_API_PORT` | API Server 绑定端口 | `19828` |
| `LLM_WIKI_CLIP_HOST` | Clip Server 绑定主机（仅用于一致性，**不建议**改成非回环） | `127.0.0.1` |
| `LLM_WIKI_CLIP_PORT` | Clip Server 绑定端口 | `19827` |
| `LLM_WIKI_API_TOKEN` | 鉴权 Token（优先级高于应用内设置） | —（回落到 `apiConfig.token`） |
| `LLM_WIKI_PUBLIC_BASE_URL` | 拼接 `agent/page` 图片直链的公网基址（优先级高于 `apiConfig.publicBaseUrl`） | 构建内置占位域名，**部署时务必覆盖为自身公网地址** |

**注意事项：**

- 空字符串视为未设置，回落到默认值；端口解析失败也回落到默认值。
- 推荐的暴露姿势是「回环绑定 + 前置反代/socat」，而非把 `LLM_WIKI_API_HOST` 直接设为 `0.0.0.0`——API 虽有 Token，但默认就不该出现在公网网卡上。
- `LLM_WIKI_PUBLIC_BASE_URL` 若不设，`agent/page` 返回的图片 URL 会指向构建内置的占位域名；**生产部署应显式设置为自己的公网地址**。改后新返回的 URL 立即生效，无需重建数据。
- 部分配置也可写在应用状态 `app-state.json` 的 `apiConfig`（见 [§3](#3-鉴权模型)）；环境变量优先。`app-state.json` 读取带 5s 缓存。

---

## 3. 鉴权模型

鉴权仅作用于 API Server 的 `/api/v1/**`。判定顺序见 [§4](#4-通用约定) 的请求处理流程。

### 3.1 开关与三态

API Server 的可用性由 `app-state.json → apiConfig` 三个布尔位 + Token 共同决定：

| 配置项（`apiConfig.*`） | 默认 | 含义 |
|---|---|---|
| `enabled` | `true`（无配置文件时**默认开**） | 总开关。关闭后所有 `/api/v1/**` 返回 **503**（但 `/health` 仍可用）。 |
| `allowUnauthenticated` | `false` | 置 `true` 则**完全跳过鉴权**（任何人可访问）。⚠️ 仅限可信内网。 |
| `mcpEnabled` | `false` | 是否允许 MCP 接入（仅状态展示位，不影响 HTTP 路由）。 |
| `token` | — | 应用内设置的 Token；被环境变量 `LLM_WIKI_API_TOKEN` 覆盖。 |

**Token 来源（`/health.tokenSource`）：** `env`（环境变量）> `store`（app-state）> `none`（未配置）。

> 失败闭合：默认是「`enabled=true` + 无 Token」，此时 `allowUnauthenticated` 仍为 `false` → 所有受保护端点 **401**，等价于关门。

### 3.2 三种 Token 传法（任选其一）

```http
Authorization: Bearer <TOKEN>
```
```http
X-LLM-Wiki-Token: <TOKEN>
```
```
?token=<TOKEN>          # query 参数，便于浏览器取图直链
```

- Token 比较使用**常量时间**比较（防时序侧信道）。
- `/health` **永不**需要鉴权（即便 `enabled=false` 也可访问），供探活与诊断。
- 当 `allowUnauthenticated=true` 时，上述都不需要。

---

## 4. 通用约定

### 4.1 请求处理流程（判定顺序，决定你拿到哪个错误码）

```
OPTIONS                         → 204（CORS 预检，永远放行）
路径 == /health 或 /api/v1/health → 200（免鉴权、免开关）
非 OPTIONS/health 且超 120 req/s → 429  Too many requests
并发在途 > 64                    → 503  API server is busy
路径不以 /api/v1 开头             → 404  Not found
apiConfig.enabled == false      → 503  API server is disabled ...
鉴权不通过                       → 401  Unauthorized
方法非 GET/POST                  → 405  Method not allowed
路由未匹配                       → 404  Not found
请求体 > 1MB 或非 UTF-8          → 400
```

### 4.2 响应包络

- 成功：`{ "ok": true, ... }`，HTTP 200。
- 失败：`{ "ok": false, "error": "<原因>" }`，并带相应 HTTP 状态码。
- 所有响应 `Content-Type: application/json`。

### 4.3 Project ID 解析

路径里的 `{id}` 可以是以下任一，按序匹配：

1. 项目 **UUID**（`.llm-wiki/project.json` 的 `id`）；
2. 别名 **`current`**（指向当前激活项目，大小写不敏感）；
3. 项目**绝对路径**（按平台规则规范化：统一 `/`，Windows 大小写不敏感）。

匹配不到返回 **404** `Unknown project: <id>`。

### 4.4 CORS

API Server 与 Clip Server 均开启 CORS：

- `Access-Control-Allow-Origin: *`
- `Access-Control-Allow-Methods: GET, POST, OPTIONS`
- API Server 允许头：`Content-Type, Authorization, X-LLM-Wiki-Token`（预检缓存 600s）
- Clip Server 允许头：`Content-Type`

### 4.5 路径暴露范围（安全沙箱）

通过 API 只能读到项目内的**公开路径**：

- `purpose.md`、`schema.md`
- `wiki/**`
- `raw/sources/**`

任何包含 `.` 开头分段的路径（如内部状态 `.llm-wiki/`、`wiki/.draft.md`）、绝对路径、`..` 路径穿越，均被拒（**403/400**）。`safe_join` 会 canonicalize 并校验解析后路径未逃逸出项目目录。

---

## 5. API Server 端点详解

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/health` | 健康检查（免鉴权） |
| GET | `/api/v1/projects` | 项目列表 |
| GET | `/api/v1/projects/{id}/files` | 列出文件树 |
| GET | `/api/v1/projects/{id}/files/content` | 读取原始文本文件 |
| GET | `/api/v1/projects/{id}/agent/page` | 结构化页面（Agent 友好） |
| GET | `/api/v1/projects/{id}/reviews` | Review 待办项 |
| POST | `/api/v1/projects/{id}/search` | 关键词/向量检索 |
| GET | `/api/v1/projects/{id}/graph` | 知识图谱 |
| POST | `/api/v1/projects/{id}/sources/rescan` | 触发源目录重扫（写） |
| POST | `/api/v1/projects/{id}/chat` | 未实现（501） |

---

### 5.1 `GET /health` — 健康检查（免鉴权）

确认服务存活与鉴权状态。`/health` 与 `/api/v1/health` 均可。

```bash
curl $BASE/health
```
```json
{
  "ok": true,
  "status": "running",
  "version": "0.5.1",
  "authRequired": true,
  "authConfigured": true,
  "tokenSource": "store",
  "enabled": true,
  "mcpEnabled": false,
  "allowUnauthenticated": false
}
```

| 字段 | 含义 |
|---|---|
| `status` | `starting` / `running` / `port_conflict` / `error` |
| `version` | 应用版本（`CARGO_PKG_VERSION`） |
| `authRequired` | 是否需要鉴权（= `!allowUnauthenticated`） |
| `authConfigured` | 是否已配置 Token |
| `tokenSource` | `env` / `store` / `none` |
| `enabled` | API 总开关 |
| `mcpEnabled` | 是否允许 MCP |
| `allowUnauthenticated` | 是否允许免鉴权访问 |

---

### 5.2 `GET /api/v1/projects` — 项目列表

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

项目条目字段：`id` / `name` / `path` / `current`（是否当前激活）。`currentProject` 在无激活项目时为 `null`。列表来源合并自项目注册表、最近项目、Clip Server 已知项目，按路径去重。

---

### 5.3 `GET /api/v1/projects/{id}/files` — 列出文件树

| Query | 默认 | 说明 |
|---|---|---|
| `root` | `wiki` | `wiki` / `sources`（=`raw/sources`，亦接受 `raw`、`raw/sources`）/ `all`（公开根集合：`purpose.md`、`schema.md`、`wiki`、`raw/sources`）。其它值 → **400** |
| `recursive` | `true` | `false` 关闭递归 |
| `maxFiles` | `2000` | 节点总数上限（含目录），范围 1–10000；超出 → **413** |

```bash
curl -G $BASE/api/v1/projects/current/files -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "root=wiki" --data-urlencode "recursive=false"
```
```json
{
  "ok": true, "projectId": "<project-uuid>", "root": "wiki", "truncated": false,
  "files": [
    { "name": "entities", "path": "wiki/entities", "isDir": true,  "size": null, "children": null },
    { "name": "overview.md", "path": "wiki/overview.md", "isDir": false, "size": 14342, "children": null }
  ]
}
```

文件节点（camelCase）：`name` / `path`（项目相对）/ `isDir` / `size`（目录为 `null`）/ `children`（递归时为子节点数组，否则 `null`）。**隐藏文件（`.` 开头）与符号链接被跳过。** 目录排序为「目录在前，再按名」。

---

### 5.4 `GET /api/v1/projects/{id}/files/content` — 读取原始文本文件

返回某个文本文件的**原始未解析**内容。多数场景用 `/agent/page` 更省事。

| Query | 必填 | 说明 |
|---|---|---|
| `path` | 是 | 公开文本路径（缺失 → 400；非公开 → 403；非文本扩展 → 415） |

```bash
curl -G $BASE/api/v1/projects/current/files/content -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "path=wiki/overview.md"
```
```json
{ "ok": true, "projectId": "<project-uuid>", "path": "wiki/overview.md", "content": "# 项目概览\n\n..." }
```

**限制：** 仅文本类扩展名（`md`、`mdx`、`txt`、`csv`、`json`、`yaml`、`yml`、`xml`、`html`、`htm`、`rtf`、`log`）；单文件 > 2MB → **413**；非 UTF-8 → **415**；不存在 → **404**。

---

### 5.5 `GET /api/v1/projects/{id}/agent/page` — 结构化页面（Agent 友好）⭐

把一个 wiki/source 页面解析成结构化 JSON：解析好的 frontmatter、干净正文、**开箱即用的图片 URL + 描述**、`[[wikilink]]` 列表。Agent 无需自己解析 Markdown/YAML。

| Query | 必填 | 说明 |
|---|---|---|
| `path` | 是 | 页面项目相对路径，取自 `/search` 的 `results[].path`。含 `%` 时务必正确 URL-encode 传入（`--data-urlencode`） |

```bash
curl -G $BASE/api/v1/projects/current/agent/page -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "path=wiki/entities/<page-slug>.md"
```
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
    "frontmatter": { "created": "2026-01-01", "type": "entity", "title": "...", "sources": ["..."] },
    "body": "# <页面标题>\n\n正文 Markdown 全文…",
    "images": [
      {
        "url": "$PUBLIC_BASE/wiki/media/<slug>/001-<file>.jpg",
        "description": "图片的文字描述（入库时注入的 alt）…",
        "path": "wiki/media/<slug>/001-<file>.jpg"
      }
    ],
    "wikilinks": ["<关联实体 A>", "<关联实体 B>"]
  }
}
```

`page` 字段：

| 字段 | 说明 |
|---|---|
| `path` | 回显请求路径 |
| `type` | 页面类型（见 [§7](#7-数据模型与页面类型)），取自 frontmatter `type`，缺省 `null` |
| `title` | 页面标题（frontmatter `title` 或正文首个 `#` 标题或文件名） |
| `tags` / `related` / `sources` | 取自 frontmatter（数组；缺省 `[]`），便于关联检索 |
| `frontmatter` | 完整 frontmatter 原样透传（行内 `[a, b]` 解析为数组，其余为去引号字符串） |
| `body` | 去掉 frontmatter 后的正文 Markdown 全文 |
| `images[]` | 每项 `{ url, description, path }`，见下 |
| `wikilinks[]` | 正文中 `[[实体]]` 列表（取 `|` 前部分，按出现序） |

`images[]`：
- `url`：**可直接 GET 的完整链接**（服务端已逐段 percent-encode，前缀为 `LLM_WIKI_PUBLIC_BASE_URL`，详见 [§8](#8-图片取用说明)）。`http(s)://` 外链原样返回。
- `description`：该图的文字描述（入库时注入的 `![alt]`）。
- `path`：原始项目相对路径（`media/…` 会被归一为 `wiki/media/…`）。
- 已按解析后 `path` 去重，保留首次出现。

**错误：** 同 `files/content`（缺 `path`→400，非公开→403，非文本→415，>2MB→413，不存在→404，非 UTF-8→415）。

---

### 5.6 `GET /api/v1/projects/{id}/reviews` — Review 待办项

列出「Review」标签页里的待办（缺页、重复、矛盾、确认、建议等），供 Agent 协助管理知识库审校。

| Query | 默认 | 说明 |
|---|---|---|
| `status` | `unresolved` | `unresolved`（亦接受 `pending`）/ `resolved` / `all`；非法值 → **400** |
| `type` | — | 按条目类型过滤，如 `missing-page` / `duplicate` / `contradiction` / `confirm` / `suggestion` |
| `limit` | `200` | 上限，范围 1–1000 |

```bash
curl -G $BASE/api/v1/projects/current/reviews -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "status=unresolved" --data-urlencode "limit=50"
```
```json
{
  "ok": true, "projectId": "<project-uuid>", "status": "unresolved", "count": 1,
  "reviews": [
    {
      "id": "r1",
      "type": "missing-page",
      "title": "Missing page: <实体名>",
      "description": "建议补充该实体页",
      "sourcePath": "raw/sources/<源文件>.md",
      "affectedPages": ["wiki/concepts/<页面>.md"],
      "searchQueries": ["<建议检索词>"],
      "options": [ { "label": "创建页面", "action": "create" } ],
      "resolved": false,
      "resolvedAction": "",
      "createdAt": 1717050000000
    }
  ]
}
```

> 响应中的条目字段经过**白名单清洗**（仅保留上列字段），`review.json` 中的其它内部字段不会外泄。`review.json` 不存在时返回空列表。

---

### 5.7 `POST /api/v1/projects/{id}/search` — 关键词/向量检索 ⭐

知识库主入口。按查询返回最相关的 wiki/source 页面。

**请求体**（`application/json`）：

| 字段 | 类型 | 必填 | 说明 |
|---|---|---|---|
| `query` | string | 是 | 查询词（空白串 → 400） |
| `topK` | int | 否 | 返回条数，默认 10，范围 1–50 |
| `includeContent` | bool | 否 | 是否附带页面全文，默认 false |
| `queryEmbedding` | number[] | 否 | 客户端自带查询向量（一般不用；服务端启用 `embeddingConfig` 时也会自动算向量） |

```bash
curl -X POST $BASE/api/v1/projects/current/search \
  -H "Authorization: Bearer $TOKEN" -H "Content-Type: application/json" \
  -d '{"query":"<查询词>","topK":5,"includeContent":false}'
```
```json
{
  "ok": true,
  "projectId": "<project-uuid>",
  "mode": "keyword",
  "note": "Search uses the shared backend retrieval service. When embeddingConfig is enabled, the API automatically includes LanceDB vector results; clients may also pass queryEmbedding explicitly.",
  "tokenHits": 42,
  "vectorHits": 0,
  "results": [
    {
      "path": "wiki/entities/<page-slug>.md",
      "title": "<页面标题>",
      "snippet": "...命中片段...",
      "titleMatch": true,
      "score": 69.0,
      "images": [ { "alt": "图片描述…", "url": "media/<slug>/001-<file>.jpg" } ]
    }
  ]
}
```

字段说明：

- `mode`：`keyword`（纯关键词）/ `hybrid`（关键词+向量，需服务端启用 `embeddingConfig`）。
- `tokenHits` / `vectorHits`：关键词命中数 / 向量命中数。
- `results[]`（camelCase）：
  - `path` → **传给 `/agent/page` 取详情**。
  - `title` / `snippet`：标题与摘要片段。
  - `titleMatch`：是否标题命中。
  - `score`：综合得分（越大越相关）；`vectorScore`：向量得分（无则该字段省略）。
  - `images[]`：`{ alt, url }`——`alt` 是图片描述，`url` 是**原始相对路径，不可直接取图**（取图请走 `/agent/page`，见 [§8](#8-图片取用说明)）。
  - `content`：`includeContent=true` 时为全文，否则省略。

> JSON 解析失败 → **400**；检索后端异常 → **500**。

---

### 5.8 `GET /api/v1/projects/{id}/graph` — 知识图谱

返回 wiki 实体间关系图（节点 + 边），用于「相关实体」扩展。

| Query | 默认 | 说明 |
|---|---|---|
| `q` | — | 按 id/label 子串过滤（大小写不敏感） |
| `nodeType` | — | 按类型过滤（`source` / `entity` / `concept` …，大小写不敏感） |
| `limit` | `200` | 节点上限，范围 1–1000 |

```bash
curl -G $BASE/api/v1/projects/current/graph -H "Authorization: Bearer $TOKEN" \
  --data-urlencode "nodeType=entity" --data-urlencode "limit=50"
```
```json
{
  "ok": true, "projectId": "<project-uuid>",
  "nodes": [
    { "id": "<node-id>", "label": "<节点标题>", "nodeType": "entity", "path": "wiki/entities/...md", "linkCount": 10 }
  ],
  "edges": [ { "source": "<nodeId>", "target": "<nodeId>", "weight": 1.0 } ]
}
```

- 节点字段（camelCase）：`id` / `label` / `nodeType` / `path` / `linkCount`。
- `type == query` 的页面**不进入**图谱。
- 先按 `q`/`nodeType` 过滤、再 `truncate(limit)`；**只保留两端节点都在结果集内的边**。

---

### 5.9 `POST /api/v1/projects/{id}/sources/rescan` — 触发源目录重扫（写）

让知识库重新扫描源目录并增量入库，使用用户的 Source Watch 规则。一般由管理员/自动化触发。

```bash
curl -X POST $BASE/api/v1/projects/current/sources/rescan -H "Authorization: Bearer $TOKEN"
```
```json
{ "ok": true, "projectId": "<project-uuid>", "result": { /* 重扫结果统计 */ } }
```

> 后端异常 → **500**。

---

### 5.10 `POST /api/v1/projects/{id}/chat` — 未实现

当前返回 **501**。聊天/RAG 管线仍在 WebView 内，尚未在本 HTTP API 暴露。请自行用 `search` + `agent/page` 组装上下文，在你自己的 LLM 里生成。

---

## 6. Clip Server 端点详解

- 基址：`http://127.0.0.1:19827`（**无前缀、无鉴权**）。
- 用途：浏览器扩展把网页转 Markdown 后 `POST /clip`；应用前端轮询 `GET /clips/pending` 自动入库。
- ⚠️ 无鉴权且写文件，**只能本机访问**。

| 方法 | 路径 | 说明 |
|---|---|---|
| GET | `/status` | 探活，返回 `{"ok":true,"version":"0.1.0"}`（此 `version` 为剪藏协议号，非应用版本） |
| GET | `/project` | 当前项目路径：`{ "ok": true, "path": "<项目绝对路径>" }` |
| POST | `/project` | 设置当前项目，body `{ "path": "<路径>" }` → `{"ok":true}`；缺 `path`/非法 JSON → 400 |
| GET | `/projects` | 已知项目：`{ "ok": true, "projects": [ { "name", "path", "current" } ] }` |
| POST | `/projects` | 批量替换已知项目，body `{ "projects": [ { "name", "path" } ] }` → `{"ok":true}` |
| GET | `/clips/pending` | 取并**清空**待入库剪藏：`{ "ok": true, "clips": [ { "projectPath", "filePath" } ] }` |
| POST | `/clip` | 写入一条剪藏（见下） |

**`POST /clip` 请求体：**

| 字段 | 必填 | 说明 |
|---|---|---|
| `content` | 是 | 正文 Markdown（空 → 400） |
| `title` | 否 | 标题，默认 `Untitled`，用于生成文件名 slug |
| `url` | 否 | 原始网址，写入 frontmatter |
| `projectPath` | 否 | 目标项目路径；缺省回落到 `POST /project` 设过的当前项目（都没有 → 400） |

```bash
curl -X POST http://127.0.0.1:19827/clip -H "Content-Type: application/json" \
  -d '{"title":"<标题>","url":"https://example.com/x","content":"# 正文\n..."}'
```
```json
{ "ok": true, "path": "raw/sources/<标题slug>-20260626.md" }
```

写入位置 `raw/sources/<slug>-<YYYYMMDD>.md`（重名自动追加 `-2`、`-3`…），并加入 `clips/pending` 队列等待前端入库。未知路径 → **404** `{"ok":false,"error":"Not found"}`。

---

## 7. 数据模型与页面类型

`page.type` / `graph.nodeType` 取值（`wiki/` 下生成页）：

| type | 目录 | 含义 |
|---|---|---|
| `overview` | `wiki/overview.md` | 项目总览 |
| `source` | `wiki/sources/` | 单个源文件分析页，**含来源图片** |
| `entity` | `wiki/entities/` | 实体页 |
| `concept` | `wiki/concepts/` | 概念页 |
| `comparison` | `wiki/comparisons/` | 对比页 |
| `synthesis` | `wiki/synthesis/` | 综合/主题页 |
| `query` | `wiki/queries/` | 检索问答页（**图谱中已过滤**） |

`raw/sources/` 下的锚点 md 类型由文件名体现，例如 `product.md` / `series.md` / `brand.md` / `case.md` / `campaign.md` / `material.md` / `catalog.md` / `brochure.md` / `guide.md` / `custom.md`（具体取决于项目的 `schema.md`）。

---

## 8. 图片取用说明

知识库图片落盘在 `wiki/media/<源路径slug>/<文件名>`，由前置反向代理对外提供。

| 来源 | 字段 | 可否直接取图 |
|---|---|---|
| `GET /agent/page` | `images[].url` | ✅ 完整链接、已编码，**直接 GET** |
| `POST /search` | `images[].url` | ❌ 原始相对路径（如 `media/.../001.jpg`），仅预览参考 |

**为何 `agent/page` 的 url 可能出现 `%2520` 这类双重编码：** media 目录名落盘时本身可能含字面 `%20` / `%XX`（源路径被 URL-encode 进了目录名）。要让反向代理解码后精确命中文件，URL 必须把字面 `%` 再编码成 `%25`。这层编码已由**服务端**逐段完成（unreserved 集 `A-Za-z0-9-_.~` 保留，其余按字节 `%XX`）——客户端**原样 GET 即可**，不要再自行 encode/decode。

```bash
# 直接用 agent/page 返回的 url 取图（无需任何额外处理）
curl -o image.jpg "$PUBLIC_BASE/wiki/media/<slug>/001-<file>.jpg"
# → 200 image/jpeg
```

> `$PUBLIC_BASE` 即 `LLM_WIKI_PUBLIC_BASE_URL`（见 [§2](#2-监听与部署配置环境变量)）。务必在部署时设置为自己的公网地址，否则 URL 会指向构建内置的占位域名。

---

## 9. 错误码

| 状态码 | 含义 |
|---|---|
| `200` | 成功 |
| `204` | OPTIONS 预检 |
| `400` | 参数缺失/非法、JSON 解析失败、`root` 非法、路径穿越、请求体 >1MB 或非 UTF-8 |
| `401` | 未鉴权 / Token 错误 |
| `403` | 路径不在公开范围内 |
| `404` | 项目不存在 / 文件不存在 / 路由未匹配 |
| `405` | 方法不允许（非 GET/POST） |
| `413` | 文件 >2MB，或文件列表超过 `maxFiles` |
| `415` | 非文本扩展 / 非 UTF-8 文件 |
| `429` | 触发限流（>120 req/s） |
| `500` | 服务端内部错误 |
| `501` | 端点未实现（`/chat`） |
| `503` | API 在设置中被禁用，或在途请求过多（>64） |

错误体统一为 `{ "ok": false, "error": "<原因>" }`。

---

## 10. 配额与限制

| 项 | 值 |
|---|---|
| 请求体上限 | 1 MB |
| 单文件读取上限 | 2 MB |
| `search.topK` | 1–50（默认 10） |
| `files.maxFiles` | 1–10000（默认 2000） |
| `reviews.limit` | 1–1000（默认 200） |
| `graph.limit` | 1–1000（默认 200） |
| 限流 | 120 请求 / 秒（滑动窗口，全局；`/health` 与 `OPTIONS` 不计） |
| 最大在途请求 | 64（超出 503） |
| 绑定重试 | 3 次 × 2s |

---

## 11. 对外暴露与多实例

默认两个服务都只在 `127.0.0.1`。对外暴露推荐：**回环绑定 + 前置反代/socat**，只暴露 API Server，Clip Server 永远留在本机。

单机多实例（每实例独立端口 + 数据 + Token）通过 [§2](#2-监听与部署配置环境变量) 的环境变量实现。规划示例（端口/路径/域名按需替换）：

| 实例 | API（本机） | Clip（本机） | 公网域名（示例） | 数据目录（示例） |
|---|---|---|---|---|
| #1 | `127.0.0.1:19828` | `127.0.0.1:19827` | `https://wiki-a.example.com` | `/path/to/data1` |
| #2 | `127.0.0.1:19928` | `127.0.0.1:19927` | `https://wiki-b.example.com` | `/path/to/data2` |
| #3 | `127.0.0.1:20028` | `127.0.0.1:20027` | `https://wiki-c.example.com` | `/path/to/data3` |

链路：`公网域名 →(nginx 443, TLS 终止)→ socat(0.0.0.0 公网端口) → 127.0.0.1:<实例 API 端口>`。

> 完整的端口/DISPLAY/域名规划、nginx 配置、socat 转发、扩容步骤见 [`../deploy/MULTI.md`](../deploy/MULTI.md)。

---

## 12. MCP / 客户端 SDK 对应关系

仓库自带的 **MCP server**（`mcp-server/`）是本 API 的薄客户端，通过 `LLM_WIKI_API_BASE_URL`（默认 `http://127.0.0.1:19828`）与 `LLM_WIKI_API_TOKEN` 连接。其工具与本文档端点一一对应：

| MCP 工具 | 对应端点 |
|---|---|
| `llm_wiki_status` | `GET /health` + `GET /api/v1/projects` |
| `llm_wiki_projects` | `GET /api/v1/projects` |
| `llm_wiki_files` | `GET /api/v1/projects/{id}/files` |
| `llm_wiki_read_file` | `GET /api/v1/projects/{id}/files/content` |
| `llm_wiki_reviews` | `GET /api/v1/projects/{id}/reviews` |
| `llm_wiki_search` | `POST /api/v1/projects/{id}/search` |
| `llm_wiki_graph` | `GET /api/v1/projects/{id}/graph` |
| `llm_wiki_rescan_sources` | `POST /api/v1/projects/{id}/sources/rescan` |

> MCP 工具默认 `project_id = "current"`；`top_k` / `max_files` / `limit` 等会被本地 API 钳制到上表区间。

---

*本文档随源码演进。若行为与实现不一致，以 `src-tauri/src/api_server.rs`、`src-tauri/src/clip_server.rs` 为准。*
