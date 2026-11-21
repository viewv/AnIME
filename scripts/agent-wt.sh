#!/usr/bin/env bash
# agent-wt.sh — 基于任务单创建/列出/清理隔离 git worktree，并统一共享 CARGO_TARGET_DIR
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
用法:
  ./scripts/agent-wt.sh create <task-id>   根据任务单 worktree 字段创建隔离分支与工作区
  ./scripts/agent-wt.sh list               列出当前所有 git worktree
  ./scripts/agent-wt.sh remove <task-id>   移除任务单对应的 git worktree
EOF
  exit 2
}

[ $# -ge 1 ] || usage
CMD=$1
shift

if ! REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null); then
  REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
fi

resolve_task_file() {
  local raw=$1
  if [ -f "$raw" ]; then
    echo "$raw"
  elif [ -f "$REPO_ROOT/.agents/tasks/$raw.md" ]; then
    echo "$REPO_ROOT/.agents/tasks/$raw.md"
  elif [ -f "$REPO_ROOT/.agents/tasks/$raw" ]; then
    echo "$REPO_ROOT/.agents/tasks/$raw"
  else
    echo "找不到任务单: $raw" >&2
    exit 2
  fi
}

read_task_meta() {
  local task_file=$1
  python3 -I - "$task_file" <<'PY'
import sys

lines = open(sys.argv[1], encoding="utf-8").read().splitlines()
if not lines or lines[0].strip() != "---":
    raise SystemExit("缺少 frontmatter")
meta = {}
for line in lines[1:]:
    if line.strip() == "---":
        break
    if ":" in line and not line[0].isspace():
        k, v = line.split(":", 1)
        v = v.strip().strip('"').strip("'")
        meta[k.strip()] = v
print(meta.get("task_id", ""))
print(meta.get("worktree", ""))
PY
}

case "$CMD" in
  list)
    git -C "$REPO_ROOT" worktree list
    ;;
  create)
    [ $# -eq 1 ] || usage
    if ! git -C "$REPO_ROOT" rev-parse --verify HEAD >/dev/null 2>&1; then
      echo "错误: 仓库尚无初始提交（unborn HEAD），请先在 main 创建初始提交后再建 worktree。" >&2
      exit 1
    fi
    TASK_FILE=$(resolve_task_file "$1")
    mapfile -t META < <(read_task_meta "$TASK_FILE")
    TASK_ID=${META[0]}
    WT_RAW=${META[1]}
    [ -n "$TASK_ID" ] && [ -n "$WT_RAW" ] || { echo "任务单缺少 task_id 或 worktree 字段: $TASK_FILE" >&2; exit 2; }

    if [[ "$WT_RAW" = /* ]]; then
      WT_PATH=$WT_RAW
    else
      WT_PATH=$(python3 -c 'import os, sys; print(os.path.realpath(os.path.join(sys.argv[1], sys.argv[2])))' "$REPO_ROOT" "$WT_RAW")
    fi

    BRANCH="feat/$TASK_ID"
    if [ -e "$WT_PATH" ]; then
      echo "工作区已存在: $WT_PATH" >&2
      exit 1
    fi

    if git -C "$REPO_ROOT" show-ref --verify --quiet "refs/heads/$BRANCH"; then
      git -C "$REPO_ROOT" worktree add "$WT_PATH" "$BRANCH"
    else
      git -C "$REPO_ROOT" worktree add "$WT_PATH" -b "$BRANCH"
    fi

    export CARGO_TARGET_DIR="$REPO_ROOT/target"
    TASK_REL=$(python3 -c 'import os, sys; print(os.path.relpath(os.path.realpath(sys.argv[1]), sys.argv[2]))' "$TASK_FILE" "$REPO_ROOT")

    echo ""
    echo "Worktree 已就绪: $WT_PATH (分支 $BRANCH)"
    echo "请在子 Agent 环境中设置共享编译缓存:"
    echo "  export CARGO_TARGET_DIR=\"$REPO_ROOT/target\""
    echo ""
    echo "启动 Prompt:"
    echo "  请严格按照 AGENTS.md 的冷启动协议，完成 $TASK_REL。"
    ;;
  remove)
    [ $# -eq 1 ] || usage
    TASK_FILE=$(resolve_task_file "$1")
    mapfile -t META < <(read_task_meta "$TASK_FILE")
    TASK_ID=${META[0]}
    WT_RAW=${META[1]}
    [ -n "$WT_RAW" ] || { echo "任务单缺少 worktree 字段: $TASK_FILE" >&2; exit 2; }
    if [[ "$WT_RAW" = /* ]]; then
      WT_PATH=$WT_RAW
    else
      WT_PATH=$(python3 -c 'import os, sys; print(os.path.realpath(os.path.join(sys.argv[1], sys.argv[2])))' "$REPO_ROOT" "$WT_RAW")
    fi
    git -C "$REPO_ROOT" worktree remove "$WT_PATH"
    echo "已移除 worktree: $WT_PATH（分支 feat/$TASK_ID 如已合并可自行 git branch -d feat/$TASK_ID）"
    ;;
  -h|--help)
    usage
    ;;
  *)
    usage
    ;;
esac
