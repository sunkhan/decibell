#!/usr/bin/env bash
# Stack an agent's commits onto the landing tree (`.worktrees/land`, branch `wip/land`), the step before the
# main tree is fast-forwarded (HANDOFF §1a). Never pushes, never touches the main tree's files.
#
#   tools/land.sh pick <wip-branch> [--skip <sha>]...   cherry-pick every commit of the branch that the landing
#                                                        tree doesn't hold yet, oldest first; stops at a conflict
#   tools/land.sh both <file>...                         resolve a conflict by keeping both sides (lists, tables)
#   tools/land.sh lock                                   resolve gpui-client/Cargo.lock: ours, then regenerated
#   tools/land.sh continue                               go on after resolving (files must be `git add`ed)
#   tools/land.sh status                                 what is left to pick
#
# Then, in the main tree: `git merge --ff-only wip/land`, build, test, push.
set -euo pipefail

COMMON=$(cd "$(dirname "$0")" && cd "$(git rev-parse --git-common-dir)" && pwd)
LAND="$(dirname "$COMMON")/.worktrees/land"
[ -d "$LAND" ] || { echo "no landing tree at $LAND (git worktree add .worktrees/land -b wip/land gpui-client)" >&2; exit 1; }
QUEUE="$(git -C "$LAND" rev-parse --absolute-git-dir)/land-queue"
G=(git -C "$LAND" -c user.name=sunkhan -c user.email=gunhanserhat@gmail.com -c core.editor=true)

drain() {
  while [ -s "$QUEUE" ]; do
    sha=$(head -n1 "$QUEUE")
    if ! "${G[@]}" cherry-pick --allow-empty --keep-redundant-commits "$sha" >/dev/null 2>&1; then
      if [ -z "$("${G[@]}" diff --name-only --diff-filter=U)" ] && [ -z "$("${G[@]}" status --porcelain --untracked-files=no)" ]; then
        "${G[@]}" cherry-pick --skip >/dev/null 2>&1 || true   # nothing left of it: already on the branch
        echo "empty   $("${G[@]}" log --oneline -1 "$sha" | cut -c1-100)"
      else
        echo "CONFLICT at $("${G[@]}" log --oneline -1 "$sha" | cut -c1-100)"
        "${G[@]}" diff --name-only --diff-filter=U | sed 's/^/  /'
        echo "resolve, git add, then: tools/land.sh continue   ($(($(wc -l <"$QUEUE") - 1)) more queued)"
        exit 2
      fi
    else
      echo "picked  $("${G[@]}" log --oneline -1 | cut -c1-100)"
    fi
    sed -i 1d "$QUEUE"
  done
  rm -f "$QUEUE"
  echo "landing tree at $("${G[@]}" log --oneline -1 | cut -c1-80)"
}

case "${1:-}" in
  pick)
    branch=${2:?branch}; shift 2
    skips=()
    while [ $# -gt 0 ]; do [ "$1" = --skip ] && { skips+=("$(git -C "$LAND" rev-parse "$2")"); shift 2; } || { echo "unknown argument $1" >&2; exit 1; }; done
    [ -s "$QUEUE" ] && { echo "a pick is in progress ($(wc -l <"$QUEUE") queued): continue it or delete $QUEUE" >&2; exit 1; }
    # `git cherry` marks with `-` what the landing tree already holds as an equal patch.
    "${G[@]}" cherry HEAD "$branch" | while read -r mark sha; do
      [ "$mark" = + ] || continue
      for s in "${skips[@]:-}"; do [ "$s" = "$sha" ] && continue 2; done
      echo "$sha"
    done >"$QUEUE"
    echo "$(wc -l <"$QUEUE") commit(s) to pick from $branch"
    drain ;;
  continue)
    if [ -n "$("${G[@]}" diff --name-only --diff-filter=U)" ]; then echo "still unresolved:" >&2; "${G[@]}" diff --name-only --diff-filter=U >&2; exit 1; fi
    "${G[@]}" cherry-pick --continue >/dev/null 2>&1 || "${G[@]}" cherry-pick --skip >/dev/null 2>&1 || true
    [ -f "$QUEUE" ] && sed -i 1d "$QUEUE"
    drain ;;
  both)
    shift
    for f in "$@"; do
      python3 -I - "$LAND/$f" <<'EOF'
import sys, re
p = sys.argv[1]
s = open(p, encoding="utf-8").read()
s = re.sub(r"^(<<<<<<< .*|=======|>>>>>>> .*|\|\|\|\|\|\|\| .*)\n", "", s, flags=re.M)
open(p, "w", encoding="utf-8").write(s)
EOF
      "${G[@]}" add -- "$f"; echo "kept both sides: $f (read it: duplicate lines are yours to remove)"
    done ;;
  lock)
    "${G[@]}" checkout --ours -- gpui-client/Cargo.lock
    (cd "$LAND/gpui-client" && cargo metadata --offline --format-version 1 >/dev/null)
    "${G[@]}" add -- gpui-client/Cargo.lock; echo "lock regenerated from ours" ;;
  status)
    [ -s "$QUEUE" ] && { echo "$(wc -l <"$QUEUE") queued:"; while read -r s; do "${G[@]}" log --oneline -1 "$s" | cut -c1-100; done <"$QUEUE"; } || echo "nothing queued"
    "${G[@]}" status --short | { grep -v '^??' || true; } | head -20 ;;
  *) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 1 ;;
esac
