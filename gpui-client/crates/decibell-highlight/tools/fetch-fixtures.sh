#!/usr/bin/env bash
# Fetches highlight.js's own test inputs (test/markup, test/detect) for the
# pinned version into tests/corpus/hljs/ (gitignored). They are BSD-3-Clause
# test data from https://github.com/highlightjs/highlight.js; tools/vectors.mjs
# turns them into differential test vectors.
set -euo pipefail
TAG=11.11.2
SHA256=21342da442ea70c6032aa8c2abab63b5a59dfbbc63726072af7630e68d23bd60
here="$(cd "$(dirname "$0")" && pwd)"
dest="$here/../tests/corpus/hljs"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
curl -sSL --fail -o "$tmp/hljs.tar.gz" "https://github.com/highlightjs/highlight.js/archive/refs/tags/$TAG.tar.gz"
echo "$SHA256  $tmp/hljs.tar.gz" | sha256sum -c - >/dev/null
mkdir -p "$tmp/x"
tar -xzf "$tmp/hljs.tar.gz" -C "$tmp/x" --wildcards '*/test/markup/*' '*/test/detect/*' '*/LICENSE'
rm -rf "$dest"
mkdir -p "$dest"
mv "$tmp/x/highlight.js-$TAG/test/markup" "$dest/markup"
mv "$tmp/x/highlight.js-$TAG/test/detect" "$dest/detect"
mv "$tmp/x/highlight.js-$TAG/LICENSE" "$dest/LICENSE"
# only the inputs are used; the expected HTML is highlight.js's, not lowlight's
find "$dest/markup" -name '*.expect.txt' -delete
echo "$TAG" > "$dest/TAG"
echo "highlight.js $TAG fixtures in $dest"
