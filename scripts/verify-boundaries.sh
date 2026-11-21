#!/usr/bin/env bash
# verify-boundaries.sh — 校验当前仓库（或指定路径列表）改动是否严格落在任务单允许边界内
#
# 用法:
#   ./scripts/verify-boundaries.sh <task-file.md> [--base <git-ref>] [--tracked-only] [--check-paths <p1> ...]
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
用法: verify-boundaries.sh <task-file.md> [--base <git-ref>] [--tracked-only] [--check-paths <p1> ...]
  --base <git-ref>     额外对比 <git-ref>...HEAD 的已提交改动（用于 worktree 分支）
  --tracked-only       忽略未跟踪文件（仅检查 git 跟踪/暂存文件改动）
  --check-paths ...    直接校验给定路径列表（供自测/预检，不读 git status）
EOF
  exit 2
}

[ $# -ge 1 ] || usage
TASK_FILE=$1
shift

BASE_REF=""
TRACKED_ONLY=0
CHECK_MODE=0
CHECK_PATHS=()

while [ $# -gt 0 ]; do
  case "$1" in
    --base)
      [ $# -ge 2 ] || usage
      BASE_REF=$2
      shift 2
      ;;
    --tracked-only)
      TRACKED_ONLY=1
      shift
      ;;
    --check-paths)
      CHECK_MODE=1
      shift
      CHECK_PATHS=("$@")
      break
      ;;
    -h|--help)
      usage
      ;;
    *)
      echo "未知参数: $1" >&2
      usage
      ;;
  esac
done

if ! REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null); then
  REPO_ROOT=$(cd "$(dirname "$0")/.." && pwd)
fi
[ -f "$TASK_FILE" ] || { echo "找不到任务单: $TASK_FILE" >&2; exit 2; }

exec python3 -I - "$REPO_ROOT" "$TASK_FILE" "$BASE_REF" "$TRACKED_ONLY" "$CHECK_MODE" "${CHECK_PATHS[@]}" <<'PY'
import os
import re
import subprocess
import sys

repo_root = os.path.realpath(sys.argv[1])
task_arg = sys.argv[2]
base_ref = sys.argv[3]
tracked_only = sys.argv[4] == "1"
check_mode = sys.argv[5] == "1"
check_paths = sys.argv[6:]


def strip_dot_slash(p: str) -> str:
    p = p.replace("\\", "/")
    while p.startswith("./"):
        p = p[2:]
    return p


def rel_repo_path(p: str) -> str:
    if os.path.isabs(p):
        rel = os.path.relpath(os.path.realpath(p), repo_root)
    else:
        rel = os.path.relpath(os.path.realpath(os.path.abspath(p)), repo_root)
    return strip_dot_slash(rel.replace(os.sep, "/"))


def strip_quotes_and_comment(s: str) -> str:
    s = s.strip()
    if not s:
        return ""
    q = s[0]
    if q in ("'", '"'):
        out = []
        i = 1
        while i < len(s):
            ch = s[i]
            if q == '"' and ch == "\\" and i + 1 < len(s):
                out.append(s[i + 1])
                i += 2
                continue
            if ch == q:
                return "".join(out)
            out.append(ch)
            i += 1
        return "".join(out)
    for i, ch in enumerate(s):
        if ch == "#" and (i == 0 or s[i - 1].isspace()):
            return s[:i].rstrip()
    return s


def parse_frontmatter(path: str) -> dict:
    lines = open(path, encoding="utf-8").read().splitlines()
    if not lines or lines[0].strip() != "---":
        raise ValueError(f"{path}: 缺少 YAML frontmatter 起始 ---")
    end_idx = None
    for idx in range(1, len(lines)):
        if lines[idx].strip() == "---":
            end_idx = idx
            break
    if end_idx is None:
        raise ValueError(f"{path}: 缺少 YAML frontmatter 结束 ---")

    data = {}
    cur_list_key = None
    for raw in lines[1:end_idx]:
        stripped = raw.strip()
        if not stripped or stripped.startswith("#"):
            continue
        if stripped.startswith("- ") or stripped == "-":
            if cur_list_key is None:
                raise ValueError(f"{path}: frontmatter 列表项缺少父键: {raw}")
            item = strip_quotes_and_comment(stripped[1:])
            if item:
                data[cur_list_key].append(item)
            continue
        if ":" in raw and not raw[0].isspace():
            cur_list_key = None
            k, v = raw.split(":", 1)
            k = k.strip()
            v_clean = strip_quotes_and_comment(v)
            if v_clean == "":
                data[k] = []
                cur_list_key = k
            elif v_clean.startswith("[") and v_clean.endswith("]"):
                inner = v_clean[1:-1].strip()
                items = [strip_quotes_and_comment(x) for x in inner.split(",")] if inner else []
                data[k] = [x for x in items if x]
            else:
                data[k] = v_clean
    return data


def glob_to_regex(pat: str) -> re.Pattern:
    pat = pat.strip().replace("\\", "/")
    while pat.startswith("./"):
        pat = pat[2:]
    if pat.endswith("/"):
        pat += "**"
    out = ["^"]
    i = 0
    n = len(pat)
    while i < n:
        if pat[i:i + 2] == "**":
            if i + 2 < n and pat[i + 2] == "/":
                out.append("(?:.+/)?")
                i += 3
            else:
                out.append(".*")
                i += 2
        elif pat[i] == "*":
            out.append("[^/]*")
            i += 1
        elif pat[i] == "?":
            out.append("[^/]")
            i += 1
        else:
            out.append(re.escape(pat[i]))
            i += 1
    out.append("$")
    return re.compile("".join(out))


def git_lines(args: list[str]) -> list[str]:
    res = subprocess.run(
        ["git", "-C", repo_root, *args],
        capture_output=True,
        text=True,
        check=True,
    )
    return [line for line in res.stdout.splitlines() if line]


def has_head() -> bool:
    res = subprocess.run(
        ["git", "-C", repo_root, "rev-parse", "--verify", "HEAD"],
        capture_output=True,
        text=True,
    )
    return res.returncode == 0


try:
    fm = parse_frontmatter(task_arg)
except ValueError as e:
    print(f"BOUNDARY ERROR: {e}", file=sys.stderr)
    sys.exit(2)

task_id = fm.get("task_id", os.path.basename(task_arg))
allowed_raw = fm.get("allowed_paths")
forbidden_raw = fm.get("forbidden_paths") or []
if not isinstance(allowed_raw, list) or not allowed_raw:
    print(f"BOUNDARY ERROR: 任务单 {task_arg} 的 allowed_paths 必须是非空列表", file=sys.stderr)
    sys.exit(2)
if not isinstance(forbidden_raw, list):
    print(f"BOUNDARY ERROR: 任务单 {task_arg} 的 forbidden_paths 必须是列表", file=sys.stderr)
    sys.exit(2)

allowed = [(p, glob_to_regex(p)) for p in allowed_raw]
forbidden = [(p, glob_to_regex(p)) for p in forbidden_raw]
task_rel = rel_repo_path(task_arg)

candidates: list[tuple[str, str]] = []
seen: set[str] = set()

if check_mode:
    for raw in check_paths:
        norm = raw.strip().replace("\\", "/")
        if os.path.isabs(norm) or norm.startswith("./") or norm.startswith("../"):
            norm = rel_repo_path(norm)
        else:
            norm = strip_dot_slash(norm)
        if norm not in seen:
            seen.add(norm)
            candidates.append(("check", norm))
else:
    if has_head():
        if base_ref:
            for p in git_lines(["diff", "--no-renames", "--name-only", f"{base_ref}...HEAD"]):
                if p not in seen:
                    seen.add(p)
                    candidates.append(("committed", p))
        for p in git_lines(["diff", "--no-renames", "--name-only", "HEAD"]):
            if p not in seen:
                seen.add(p)
                candidates.append(("tracked", p))
    else:
        if base_ref:
            print(f"BOUNDARY ERROR: 仓库尚无初始提交（unborn HEAD），无法对比 --base {base_ref}", file=sys.stderr)
            sys.exit(2)
        for p in git_lines(["ls-files"]):
            if p not in seen:
                seen.add(p)
                candidates.append(("staged", p))
    if not tracked_only:
        for p in git_lines(["ls-files", "--others", "--exclude-standard"]):
            if p not in seen:
                seen.add(p)
                candidates.append(("untracked", p))

violations: list[str] = []
checked_count = 0
for kind, path in candidates:
    if path == task_rel:
        continue
    checked_count += 1
    hit_forbidden = next((raw for raw, rx in forbidden if rx.match(path)), None)
    if hit_forbidden is not None:
        violations.append(f"  - [{kind}] {path}  (命中 forbidden_paths: {hit_forbidden})")
        continue
    hit_allowed = next((raw for raw, rx in allowed if rx.match(path)), None)
    if hit_allowed is None:
        violations.append(f"  - [{kind}] {path}  (不在 allowed_paths 白名单内)")

if violations:
    print(f"BOUNDARY VIOLATION ({task_id}): 发现 {len(violations)} 处越界改动：", file=sys.stderr)
    for line in violations:
        print(line, file=sys.stderr)
    sys.exit(1)

print(f"BOUNDARY OK ({task_id}): 已检查 {checked_count} 个文件，零越界")
PY
