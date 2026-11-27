#!/usr/bin/env bash
# readonly-run.sh: a packaged tree run from a read-only directory (bubblewrap binds it read-only):
# `--selftest boot`, then a sign-in against the local test stack. Needs a development build with
# `dev-local` (the ship build has neither self-tests nor --login, and never talks to a test stack):
# the tree is laid out as the tarball's (resources/install.json, licences), only the binary differs.
#
#   packaging/tests/readonly-run.sh <decibell built with --features dev-local>
set -euo pipefail
bin="$(realpath "$1")"
repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
version="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$repo/electron-client/package.json")"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/decibell-ro-XXXXXX")"
trap 'rm -rf "$tmp"' EXIT
pkg="$tmp/decibell-$version-linux-x86_64"
mkdir -p "$pkg/resources" "$pkg/licenses" "$pkg/share/applications"
install -m 0755 "$bin" "$pkg/decibell"
printf '{"kind":"tarball","version":"%s"}\n' "$version" > "$pkg/resources/install.json"
cp "$repo/gpui-client/assets/linux/decibell.desktop" "$pkg/share/applications/"
before="$(cd "$pkg" && find . -exec stat -c '%n %s %Y' {} + | sort)"
chmod -R a-w "$pkg"
ro=(bwrap --dev-bind / / --ro-bind "$pkg" "$pkg" --)

echo "== boot self-test from the read-only tree"
DECIBELL_USER_DATA_DIR="$tmp/profile-boot" "${ro[@]}" "$pkg/decibell" --selftest boot 2>&1 | tail -4
echo "exit ${PIPESTATUS[0]}"

echo "== sign-in against the local stack from the read-only tree"
eval "$("$repo/tools/dev/local-stack.sh" client-env | grep '^export')"
out="$tmp/signin.log"
DECIBELL_USER_DATA_DIR="$tmp/profile-signin" DECIBELL_DEV_PASSWORD=decibell-test-1 DECIBELL_LOG=warn \
  "${ro[@]}" "$pkg/decibell" --login perf1 >"$out" 2>&1 &
app=$!
for _ in $(seq 300); do grep -q 'decibell: connected' "$out" && break; sleep 0.1; done
sleep 1
kill "$app" 2>/dev/null || true
wait "$app" 2>/dev/null || true
grep -E 'signed-in|connected|EROFS|Read-only|ERROR' "$out" | head -10
grep -q 'decibell: signed-in perf1' "$out" && echo "signed in: yes" || { echo "signed in: NO"; cat "$out" | tail -20; }
echo "== the tree is unchanged"
chmod -R u+w "$pkg"
after="$(cd "$pkg" && find . -exec stat -c '%n %s %Y' {} + | sort)"
if [ "$before" = "$after" ]; then echo "unchanged: yes ($(echo "$after" | wc -l) entries)"; else echo "CHANGED:"; diff <(echo "$before") <(echo "$after"); fi
