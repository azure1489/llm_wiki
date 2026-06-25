# 在一台服务器上运行多个 LLM Wiki 实例

LLM Wiki 是 Tauri 桌面应用，原本假设单实例（API/Clip 端口硬编码、共用一个 VNC 桌面）。
本目录把它改造成可在同机跑 **N 个互相隔离的实例**，每个实例：

- **独立数据**：各自数据卷挂到容器 `/data`（`HOME=/data`），项目/配置/聊天互不影响。
- **独立端口**：API 与 Clip 端口由 env 配置，互不冲突。
- **独立桌面**：各自一个 VNC 显示（`:1`、`:2`…），通过对应 VNC 端口（5901、5902…）远程查看。
- **API 对外暴露**：API 绑 `127.0.0.1` + 宿主 `socat` 转发到公网端口；Clip 无鉴权，仅本机。

## 端口/显示分配

| 实例 | 容器名 | DISPLAY | VNC 端口 | API(本机) | CLIP(本机) | API 公网(socat) | 数据卷 |
|---|---|---|---|---|---|---|---|
| #1 | llm-wiki-1 | :1 | 5901 | 19828 | 19827 | 19829 | /opt/llm-wiki-data |
| #2 | llm-wiki-2 | :1（共用） | 5901 | 19928 | 19927 | 19929 | /opt/llm-wiki-data-2 |

约定：实例 k 的 API=19828+100·(k-1)、CLIP=19827+100·(k-1)、公网=19829+100·(k-1)。

**桌面模式（两选一）**：
- **共用桌面（当前部署）**：所有实例 `DISPLAY=:1`，连一个 VNC 5901 即可看到全部窗口并排。简单、够用。
- **独立桌面**：实例 k 用 `DISPLAY=:k` + 启用 `llm-wiki-vnc@k.service`（VNC 5900+k）。视觉隔离，但每个实例要单独连一个 VNC 端口。
  注意：root 在 :1 已有完整 XFCE；额外桌面用轻量方案（见下「额外 VNC 桌面」），其透明标题栏在无合成器下会显示黑边（仅外观）。

## 涉及的可配 env（源码已支持）

- `LLM_WIKI_API_HOST`（默认 `127.0.0.1`）、`LLM_WIKI_API_PORT`（默认 `19828`）
- `LLM_WIKI_CLIP_HOST`（默认 `127.0.0.1`）、`LLM_WIKI_CLIP_PORT`（默认 `19827`）
- `LLM_WIKI_API_TOKEN`：API 鉴权 token（对外暴露务必设置强随机值）
- 显示/渲染：`DISPLAY`、`XAUTHORITY`、`WEBKIT_DISABLE_DMABUF_RENDERER`、`WEBKIT_DISABLE_COMPOSITING_MODE`、`LIBGL_ALWAYS_SOFTWARE`

## 启动（先 2 个）

```bash
cd /root/llm_wiki
cp deploy/.env.example deploy/.env   # 填入 openssl rand -hex 32 生成的 token
docker rm -f llm-wiki 2>/dev/null    # 移除旧的单实例容器（首次迁移）
docker compose -f deploy/docker-compose.yml up -d
```

## 额外 VNC 桌面 :2/:3

`:1` 是系统自带的 tigervnc 桌面。额外实例不能再为 root 起完整 XFCE 会话（xfce4-session
单例 + nvidia GBM 合成器会崩溃），所以用轻量方案 `deploy/vnc-extra.sh`（Xvnc + xfwm4 关合成器
+ xhost 放行本机）+ 模板单元 `llm-wiki-vnc@.service`：

```bash
cp deploy/llm-wiki-vnc@.service /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now llm-wiki-vnc@2     # → :2 / 5902
# systemctl enable --now llm-wiki-vnc@3   # → :3 / 5903
```

VNC 密码沿用 `/root/.vnc/passwd`（与 :1 相同）。

## API 对外暴露（socat）

沿用 `llm-wiki-proxy.service`：把各实例本机 API 端口转发到公网端口。新增实例时在该单元的
`ExecStart` 追加一行，例如实例 #2：

```
socat TCP-LISTEN:19929,bind=0.0.0.0,fork,reuseaddr TCP:127.0.0.1:19928 &
```

改完 `systemctl daemon-reload && systemctl restart llm-wiki-proxy`。
⚠️ 如需公网访问，记得在云安全组放行对应端口；务必为每实例设置 `LLM_WIKI_API_TOKEN`。

## 外部 agent（Claude Code / Codex / MCP）连接某个实例

MCP server 与 HTTP 客户端通过 env 选择实例，无需改代码：

```bash
export LLM_WIKI_API_BASE_URL=http://<服务器IP>:19929   # 实例 #2 的公网 API
export LLM_WIKI_API_TOKEN=<实例#2 的 token>
```

健康检查：`curl -s http://<IP>:19929/health`；鉴权调用需带 `Authorization: Bearer <token>`。

## 新增第 3 个实例（步骤）

1. `docker-compose.yml` 复制 `wiki2` 段为 `wiki3`：改 `container_name`、`DISPLAY=:3`、
   `LLM_WIKI_API_PORT=20028`、`LLM_WIKI_CLIP_PORT=20027`、数据卷 `/opt/llm-wiki-data-3`、token。
2. `.env` 增加 `WIKI3_TOKEN`。
3. `systemctl enable --now llm-wiki-vnc@3`。
4. `llm-wiki-proxy.service` 追加 `socat ...20029→127.0.0.1:20028`，重启该服务。
5. `docker compose -f deploy/docker-compose.yml up -d wiki3`。
