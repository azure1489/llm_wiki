#!/usr/bin/env bash
# 从本机把当前工作区部署到服务器上的多实例 LLM Wiki（见 deploy/MULTI.md）。
#
# 流程：预检 → 备份 .env + 给旧镜像打回滚标签 → rsync（先 dry-run，有删除则需确认）
#       → 服务器后台 docker build（断线不影响）→ 轮询构建结果 → compose 重建容器 → 健康检查。
#
# 用法：
#   bash deploy/deploy.sh               # 完整部署
#   bash deploy/deploy.sh --yes         # 跳过所有确认
#   bash deploy/deploy.sh --sync-only   # 只同步源码，不构建、不重启
#   bash deploy/deploy.sh --no-restart  # 同步 + 构建，不重启容器
#   bash deploy/deploy.sh --rollback    # 回滚到上次部署前的镜像并重启
#
# 环境变量：DEPLOY_HOST（默认 centos9c，即 ~/.ssh/config 中直连 IP 的主机；
#           centos9 走代理域名，常连不上）、DEPLOY_DIR（默认 /root/llm_wiki）。
set -euo pipefail

HOST="${DEPLOY_HOST:-centos9c}"
REMOTE_DIR="${DEPLOY_DIR:-/root/llm_wiki}"
COMPOSE="docker compose -f deploy/docker-compose.yml"
API_PORTS=(19828 19928 20028)
POLL_INTERVAL=60
POLL_MAX=180   # 最多等 3 小时

YES=0; MODE=full
for arg in "$@"; do
  case "$arg" in
    -y|--yes) YES=1 ;;
    --sync-only) MODE=sync ;;
    --no-restart) MODE=build ;;
    --rollback) MODE=rollback ;;
    -h|--help) sed -n '2,14p' "$0"; exit 0 ;;
    *) echo "未知参数：$arg" >&2; exit 2 ;;
  esac
done

cd "$(dirname "$0")/.."

log() { printf '\n\033[1;34m==> %s\033[0m\n' "$*"; }
die() { printf '\033[1;31m✗ %s\033[0m\n' "$*" >&2; exit 1; }
remote() { ssh -o BatchMode=yes -o LogLevel=ERROR -o ConnectTimeout=15 "$HOST" "$@" 2>&1 | grep -av -i 'post-quantum\|store now\|pq.html' || true; }
remote_strict() { ssh -o BatchMode=yes -o LogLevel=ERROR -o ConnectTimeout=15 "$HOST" "$@"; }
confirm() {
  [ "$YES" = 1 ] && return 0
  read -r -p "$1 [y/N] " ans
  [[ "$ans" =~ ^[Yy]$ ]] || die "已取消"
}

restart_and_check() {
  log "重建容器"
  remote_strict "cd $REMOTE_DIR && $COMPOSE up -d" 2>&1 | grep -av -i 'post-quantum\|store now\|pq.html'
  log "健康检查（等待 15s 启动）"
  sleep 15
  local failed=0
  for port in "${API_PORTS[@]}"; do
    if remote_strict "curl -fsS --max-time 10 http://127.0.0.1:$port/health" >/dev/null 2>&1; then
      echo "  ✓ :$port"
    else
      echo "  ✗ :$port 无响应"; failed=1
    fi
  done
  remote "docker ps --filter name=llm-wiki --format '  {{.Names}}\t{{.Image}}\t{{.Status}}'"
  [ "$failed" = 0 ] || die "有实例未通过健康检查，查看：ssh $HOST docker logs --tail 50 llm-wiki-N；回滚：bash deploy/deploy.sh --rollback"
}

# ---------- 回滚 ----------
if [ "$MODE" = rollback ]; then
  remote_strict "docker image inspect llm-wiki:prev >/dev/null 2>&1" || die "服务器上没有 llm-wiki:prev 镜像"
  confirm "将 llm-wiki:local 回滚到 llm-wiki:prev 并重启 3 个实例？"
  remote_strict "docker tag llm-wiki:prev llm-wiki:local"
  restart_and_check
  log "回滚完成"; exit 0
fi

# ---------- 预检 ----------
log "预检：$HOST:$REMOTE_DIR"
remote_strict "test -d $REMOTE_DIR && test -f $REMOTE_DIR/deploy/.env" \
  || die "无法连接 $HOST，或 $REMOTE_DIR/deploy/.env 不存在"
if [ -n "$(git status --porcelain)" ]; then
  echo "  ⚠ 本地有未提交改动（会一并同步）："; git status --short | sed 's/^/    /'
fi
echo "  本地版本：$(git rev-parse --short HEAD) $(node -p 'require("./package.json").version' 2>/dev/null || true)"
echo "  服务器版本：$(remote "grep -m1 '\"version\"' $REMOTE_DIR/package.json" | tr -d ' ,')"
remote_strict "pgrep -f 'docker build -t llm-wiki' >/dev/null" && die "服务器上已有构建在进行，稍后再试"
confirm "继续部署（模式：$MODE）？"

# ---------- 备份 ----------
log "备份 .env 并给当前镜像打回滚标签 llm-wiki:prev"
remote_strict "cp -a $REMOTE_DIR/deploy/.env /root/llm_wiki-env.bak.\$(date +%Y%m%d%H%M%S) \
  && docker tag llm-wiki:local llm-wiki:prev \
  && docker tag llm-wiki:local llm-wiki:prev-\$(date +%Y%m%d%H%M%S)"

# ---------- 同步 ----------
RSYNC_OPTS=(-az --delete -e "ssh -o BatchMode=yes -o LogLevel=ERROR"
  --exclude .git --exclude node_modules --exclude dist --exclude src-tauri/target
  --exclude deploy/.env --exclude '*.log' --exclude .claude --exclude .vscode
  --exclude .DS_Store --exclude '*.tsbuildinfo')

log "rsync dry-run"
DRY="$(rsync "${RSYNC_OPTS[@]}" --dry-run -i ./ "$HOST:$REMOTE_DIR/" 2>&1 | grep -av -i 'post-quantum\|store now\|pq.html' || true)"
CHANGED="$(printf '%s\n' "$DRY" | grep -ac '^<f' || true)"
DELETES="$(printf '%s\n' "$DRY" | grep -a 'deleting' || true)"
echo "  将传输 $CHANGED 个文件"
if [ -n "$DELETES" ]; then
  echo "  将删除："; printf '%s\n' "$DELETES" | sed 's/^/    /'
  printf '%s\n' "$DELETES" | grep -q 'deploy/\.env' && die "删除列表包含 deploy/.env，中止"
  confirm "确认删除以上服务器文件？"
fi

log "rsync"
rsync "${RSYNC_OPTS[@]}" ./ "$HOST:$REMOTE_DIR/" 2>&1 | grep -av -i 'post-quantum\|store now\|pq.html' || true
LOCAL_SUM="$(shasum -a 256 package.json | cut -d' ' -f1)"
REMOTE_SUM="$(remote_strict "sha256sum $REMOTE_DIR/package.json" | cut -d' ' -f1)"
[ "$LOCAL_SUM" = "$REMOTE_SUM" ] || die "同步校验失败（package.json 哈希不一致）"
echo "  ✓ 同步完成并校验"
[ "$MODE" = sync ] && { log "仅同步，结束"; exit 0; }

# ---------- 构建 ----------
BUILD_LOG="deploy/build-$(date +%Y%m%d%H%M%S).log"
log "服务器后台构建镜像，日志：$REMOTE_DIR/$BUILD_LOG"
# 关闭 ssh 的 stdin/stdout，避免 ssh 等待后台进程而挂住。
ssh -o BatchMode=yes -o LogLevel=ERROR -f "$HOST" "cd $REMOTE_DIR && nohup sh -c 'docker build -t llm-wiki:local -f deploy/Dockerfile . > $BUILD_LOG 2>&1; echo BUILD_EXIT=\$? >> $BUILD_LOG' </dev/null >/dev/null 2>&1 &" 2>/dev/null

START=$(date +%s)
for ((i = 1; i <= POLL_MAX; i++)); do
  sleep "$POLL_INTERVAL"
  RESULT="$(remote "grep -a -m1 BUILD_EXIT $REMOTE_DIR/$BUILD_LOG")"
  if [ -n "$RESULT" ]; then break; fi
  STEP="$(remote "grep -ao '^#[0-9]* \[[^]]*\]' $REMOTE_DIR/$BUILD_LOG | tail -1")"
  printf '  %3d 分钟  %s\n' $(( ($(date +%s) - START) / 60 )) "$STEP"
done
[ -n "${RESULT:-}" ] || die "构建超时，继续查看：ssh $HOST tail -f $REMOTE_DIR/$BUILD_LOG"
if [ "$RESULT" != "BUILD_EXIT=0" ]; then
  remote "tail -40 $REMOTE_DIR/$BUILD_LOG"
  remote_strict "docker tag llm-wiki:prev llm-wiki:local"
  die "构建失败（$RESULT），llm-wiki:local 已恢复为旧镜像，运行中的实例未受影响"
fi
echo "  ✓ 构建成功，用时 $(( ($(date +%s) - START) / 60 )) 分钟"
remote "docker images llm-wiki --format '  {{.Tag}}\t{{.ID}}\t{{.CreatedSince}}'"
[ "$MODE" = build ] && { log "未重启容器。需要时：bash deploy/deploy.sh --rollback 或 ssh $HOST 'cd $REMOTE_DIR && $COMPOSE up -d'"; exit 0; }

# ---------- 重启 ----------
confirm "重启 3 个实例（会打断正在进行的导入任务）？"
restart_and_check
log "部署完成。回滚：bash deploy/deploy.sh --rollback"
