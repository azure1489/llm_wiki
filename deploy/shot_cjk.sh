#!/usr/bin/env bash
# 在 VNC :1 上驱动 LLM Wiki 打开新建项目对话框+语言下拉，整屏截图，验证 CJK 字体。
# 结果：/root/llm_wiki/deploy/cjk.png ；过程日志：/root/llm_wiki/deploy/shot_cjk.log
exec > /root/llm_wiki/deploy/shot_cjk.log 2>&1
set -x
DX="docker exec llm-wiki env DISPLAY=:1 XAUTHORITY=/root/.Xauthority"

# 等主窗口出现
WID=""
for i in $(seq 1 30); do
  WID=$($DX xwininfo -root -tree 2>/dev/null | grep -E '"LLM Wiki":' | grep -oE '0x[0-9a-f]+' | head -1)
  [ -n "$WID" ] && break
  sleep 1
done
echo "WID=$WID"
[ -z "$WID" ] && { echo "NO_WINDOW"; exit 1; }

# 取窗口绝对坐标
GEO=$($DX xwininfo -id "$WID" 2>/dev/null)
AX=$(echo "$GEO" | awk '/Absolute upper-left X/{print $NF}')
AY=$(echo "$GEO" | awk '/Absolute upper-left Y/{print $NF}')
echo "abs=$AX,$AY"

$DX xdotool windowactivate "$WID"; sleep 1
# 先按 Esc 复位（关掉可能残留的对话框/下拉）
$DX xdotool key Escape; sleep 1; $DX xdotool key Escape; sleep 1
# 点 "New Project"（欢迎页中心偏下，窗口相对 ~527,450）
$DX xdotool mousemove $((AX+527)) $((AY+450)) click 1; sleep 2
# 点 "Pick a language..." 下拉（对话框内，窗口相对 600,211，由裁剪图精确量得）
$DX xdotool mousemove $((AX+600)) $((AY+211)) click 1; sleep 2

# 整屏截图（下拉弹层多为独立 override-redirect 窗口，截 root 最稳）
$DX import -window root /tmp/cjk.png
docker cp llm-wiki:/tmp/cjk.png /root/llm_wiki/deploy/cjk.png
echo DONE
