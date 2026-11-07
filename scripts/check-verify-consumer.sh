#!/usr/bin/env bash
# Checks the verify-oxsum-bill distributable the way a customer would use it
# (issue #561): build the wasm, pack the npm tarball, assert what it carries,
# install it into a clean temporary project outside the repository, and run
# the offline example against captured genuine evidence.
#
# The consumer imports `verify-oxsum-bill` by its package name only — no
# workspace paths, no bundler aliases, no server. `npm publish` remains a
# separate maintainer step; this proves the artifact we would publish.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

WORK="$(mktemp -d "${TMPDIR:-/tmp}/oxsum-verify-consumer.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

echo "== Building the wasm module =="
bash scripts/build-verify-wasm.sh

echo
echo "== Packing verify-oxsum-bill =="
if [ ! -d web/verify-sdk/node_modules ]; then
    (cd web/verify-sdk && npm ci --no-audit --no-fund)
fi
(cd web/verify-sdk && npm pack --pack-destination "$WORK")
TARBALL="$(find "$WORK" -maxdepth 1 -name 'verify-oxsum-bill-*.tgz' -print -quit)"
if [ -z "$TARBALL" ]; then
    echo "npm pack produced no tarball" >&2
    exit 1
fi
echo "tarball: $TARBALL"

echo
echo "== Checking the tarball's contents =="
LISTING="$(tar -tzf "$TARBALL")"
for required in \
    package/package.json \
    package/README.md \
    package/LICENSE-MIT \
    package/LICENSE-APACHE \
    package/dist/index.js \
    package/dist/index.d.ts \
    package/dist/verify-bill.js \
    package/dist/verify-statement.js \
    package/dist/verify-usage-report.js \
    package/wasm/verify_wasm.js \
    package/wasm/verify_wasm_bg.wasm
do
    if ! grep -qx "$required" <<<"$LISTING"; then
        echo "tarball is missing $required" >&2
        exit 1
    fi
done
VERSION="$(tar -xOf "$TARBALL" package/package.json \
    | node -e 'let s="";process.stdin.on("data",d=>s+=d).on("end",()=>console.log(JSON.parse(s).version))')"
echo "contents OK (verify-oxsum-bill@$VERSION)"

echo
echo "== Installing into a clean consumer =="
CONSUMER="$WORK/consumer"
mkdir -p "$CONSUMER"
cp examples/verify-consumer/package.json "$CONSUMER/package.json"
cp examples/verify-consumer/verify.mjs "$CONSUMER/verify.mjs"
cp -R examples/verify-consumer/fixtures "$CONSUMER/fixtures"
(
    cd "$CONSUMER"
    npm install --no-audit --no-fund "$TARBALL"
)

echo
echo "== Verifying the exported evidence offline =="
(
    cd "$CONSUMER"
    node verify.mjs
)

echo
# Retain the already installed and verified bytes, never a second npm pack.
if [ -n "${VERIFY_CONSUMER_OUTPUT:-}" ]; then
    mkdir -p "$VERIFY_CONSUMER_OUTPUT"
    destination="$VERIFY_CONSUMER_OUTPUT/$(basename "$TARBALL")"
    [ ! -e "$destination" ] || { echo "refusing to overwrite $destination" >&2; exit 1; }
    cp "$TARBALL" "$destination"
    cmp "$TARBALL" "$destination"
fi
echo "verify-oxsum-bill@$VERSION: clean install and offline verification passed."
