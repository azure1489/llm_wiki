#!/usr/bin/env bash
# 在服务器上启动 LLM Wiki 容器，GUI 渲染到已运行的 VNC 桌面 :1。
# 用法：bash deploy/run.sh
set -euo pipefail

IMAGE="${IMAGE:-llm-wiki:local}"
NAME="${NAME:-llm-wiki}"
DISPLAY_NUM="${DISPLAY_NUM:-:1}"
XAUTH="${XAUTH:-/root/.Xauthority}"   # Xvnc :1 以 -auth /root/.Xauthority 启动
DATA_DIR="${DATA_DIR:-/opt/llm-wiki-data}"

mkdir -p "$DATA_DIR"

# 移除旧容器（幂等）
docker rm -f "$NAME" >/dev/null 2>&1 || true

docker run -d --name "$NAME" --restart unless-stopped \
  --net host \
  -e DISPLAY="$DISPLAY_NUM" \
  -e XAUTHORITY="$XAUTH" \
  -e WEBKIT_DISABLE_DMABUF_RENDERER=1 \
  -e WEBKIT_DISABLE_COMPOSITING_MODE=1 \
  -e LIBGL_ALWAYS_SOFTWARE=1 \
  -v /tmp/.X11-unix:/tmp/.X11-unix \
  -v "$XAUTH":"$XAUTH":ro \
  -v "$DATA_DIR":/data \
  "$IMAGE"

echo "已启动容器 $NAME。"
echo "  · VNC 桌面查看：连接 <服务器IP>:5901 即可看到 LLM Wiki 窗口"
echo "  · 健康检查：curl -s http://127.0.0.1:19828/health"
echo "  · 日志：docker logs -f $NAME"
