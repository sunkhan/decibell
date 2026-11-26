#!/usr/bin/env bash
# Produces gpui-client/vendor/gpui-src: Zed at the pinned commit (GPUI_PIN) with every
# gpui-client/patches/*.patch applied, in lexical order. Cargo builds GPUI from there.
#
#   tools/fetch-gpui.sh           # no-op when the pin and the patches are unchanged
#   tools/fetch-gpui.sh --force   # rebuild the copy even if it has edits that are in no patch
#
# vendor/zed is the pristine clone at the pin (fetched here if missing); vendor/gpui-src is a
# git worktree of it. Patches are applied with `git apply --index`, so inside vendor/gpui-src
# `git diff` shows only what was edited since: that is how a new patch is written
# (patches/README.md). Runs under Git Bash on Windows.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
url="https://github.com/zed-industries/zed"
pin="$(tr -d '[:space:]' < "$root/GPUI_PIN")"
zed="$root/vendor/zed"
src="$root/vendor/gpui-src"
stamp="$root/vendor/gpui-src.stamp"

force=0
case "${1:-}" in
    "") ;;
    --force) force=1 ;;
    *) echo "usage: fetch-gpui.sh [--force]" >&2; exit 2 ;;
esac

shopt -s nullglob
patches=("$root"/patches/*.patch)

# What the copy was built from: the pin, then each patch's name and content hash.
want="$pin"
for patch in "${patches[@]}"; do
    want+=$'\n'"$(basename "$patch") $(git hash-object --no-filters "$patch")"
done

if [ "$force" = 0 ] && [ -d "$src" ] && [ -f "$stamp" ] && [ "$(cat "$stamp")" = "$want" ]; then
    echo "fetch-gpui: vendor/gpui-src is up to date (pin ${pin:0:12}, ${#patches[@]} patches)"
    exit 0
fi

# The pristine clone: a shallow fetch of exactly the pinned commit.
if [ ! -d "$zed/.git" ]; then
    echo "fetch-gpui: cloning $url at ${pin:0:12} into vendor/zed"
    mkdir -p "$zed"
    git -C "$zed" init -q
    git -C "$zed" remote add origin "$url"
fi
# Line endings as committed (patches are LF) and Windows' 260-character path limit lifted,
# both before anything is checked out.
git -C "$zed" config core.autocrlf false
git -C "$zed" config core.longpaths true
if ! git -C "$zed" cat-file -e "$pin^{commit}" 2>/dev/null; then
    git -C "$zed" fetch -q --depth 1 origin "$pin"
fi
if [ "$(git -C "$zed" rev-parse -q --verify HEAD 2>/dev/null || true)" != "$pin" ]; then
    if [ -n "$(git -C "$zed" status --porcelain 2>/dev/null)" ]; then
        echo "fetch-gpui: vendor/zed is not at the pin and has local changes; it must stay pristine." >&2
        exit 1
    fi
    git -C "$zed" checkout -q --detach "$pin"
fi

# Never throw away a patch that is being written.
if [ -e "$src/.git" ] && [ "$force" = 0 ] && [ -n "$(git -C "$src" status --porcelain | grep -v '^[MADR]  ' || true)" ]; then
    echo "fetch-gpui: vendor/gpui-src has edits that are in no patch:" >&2
    git -C "$src" status --short | grep -v '^[MADR]  ' >&2
    echo "Save them (cd vendor/gpui-src && git add -N . && git diff > ../../patches/NNNN-name.patch)," >&2
    echo "then run tools/fetch-gpui.sh --force to rebuild the copy from the patches." >&2
    exit 1
fi

echo "fetch-gpui: building vendor/gpui-src from ${pin:0:12} + ${#patches[@]} patches"
rm -f "$stamp"
if [ -e "$src/.git" ]; then
    git -C "$zed" worktree remove --force "$src" 2>/dev/null || true
fi
rm -rf "$src"
git -C "$zed" worktree prune
git -C "$zed" worktree add -q --detach "$src" "$pin"

for patch in "${patches[@]}"; do
    echo "  applying $(basename "$patch")"
    git -C "$src" apply --index --whitespace=nowarn "$patch"
done

printf '%s' "$want" > "$stamp"
echo "fetch-gpui: done"
