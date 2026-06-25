#!/usr/bin/env bash
# 为额外的 llm_wiki 实例提供一个独立、轻量的 VNC 桌面。
#
# 为什么不用 tigervnc 的 vncserver@:N + 完整 XFCE：
#   root 用户在 :1 已经有一个完整 xfce4-session；在 :2 再起完整 XFCE 会因
#   xfce4-session 的每用户单例 + nvidia GBM 合成器报错而崩溃，连带 Xvnc 退出。
#   实例的 Tauri 窗口只需要「一个活着的 X 服务器 + 一个轻量 WM」即可显示。
#
# 本脚本：Xvnc :N（软件渲染）+ xfwm4（关合成器）+ xhost 放行本机，
# 让 --net host 的容器以 DISPLAY=:N 连接 /tmp/.X11-unix/X<N> 渲染。
#
# 用法：vnc-extra.sh <N>      例如 vnc-extra.sh 2  →  :2, rfbport 5902
set -euo pipefail
N="${1:?usage: vnc-extra.sh <display-number>}"
DISP=":$N"
RFB=$((5900 + N))
GEOM="${GEOM:-1920x1200}"
AUTH="${XAUTH:-/root/.Xauthority}"
PASSWD="${VNC_PASSWD:-/root/.vnc/passwd}"
HOST="$(hostname)"

export LIBGL_ALWAYS_SOFTWARE=1
export HOME=/root

# 清理可能残留的锁/socket（上次异常退出留下的）
rm -f "/tmp/.X${N}-lock" "/tmp/.X11-unix/X${N}" 2>/dev/null || true

# 预置 X11 magic cookie，供宿主侧 xhost 连接 :N（容器侧改用 xhost 放行，见下）
COOKIE="$(mcookie)"
for who in "${HOST}${DISP}" "${HOST}/unix${DISP}" "${DISP}"; do
  xauth -f "$AUTH" add "$who" MIT-MAGIC-COOKIE-1 "$COOKIE" 2>/dev/null || true
done

# 启动 Xvnc：VNC 端用密码鉴权(5900+N, 监听 0.0.0.0)，X11 端用上面的 cookie。
Xvnc "$DISP" -geometry "$GEOM" -depth 24 -rfbport "$RFB" \
  -SecurityTypes VncAuth -rfbauth "$PASSWD" -localhost no -AlwaysShared \
  -auth "$AUTH" -nolisten tcp &
XVNC=$!

# 等 X 服务器就绪
for i in $(seq 1 20); do
  [ -S "/tmp/.X11-unix/X${N}" ] && break
  sleep 0.5
done

# 放行本机 root 的 X 客户端（容器经挂载的 unix socket 连接 → 视为本机连接）
DISPLAY="$DISP" xhost +SI:localuser:root 2>/dev/null || DISPLAY="$DISP" xhost +local: 2>/dev/null || true
# 轻量 WM，关合成器（否则触发 nvidia GBM 报错使 X 崩溃）
DISPLAY="$DISP" xfwm4 --compositor=off --daemon 2>/dev/null || true

# Xvnc 退出则脚本退出（交给 systemd Restart=always 拉起）
wait "$XVNC"
