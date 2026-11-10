#!/usr/bin/env bash
# Build/pack a clean verifier consumer, then run the separate application listener fixture.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
WORK="$(mktemp -d "${TMPDIR:-/tmp}/oxsum-embedded-consumer.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT
bash scripts/build-verify-wasm.sh
if [ ! -d web/verify-sdk/node_modules ]; then
    (cd web/verify-sdk && npm ci --no-audit --no-fund)
fi
(cd web/verify-sdk && npm pack --pack-destination "$WORK" > "$WORK/pack.txt")
TARBALL="$WORK/$(tail -n 1 "$WORK/pack.txt")"
[ -f "$TARBALL" ] || { echo "verifier tarball missing" >&2; exit 1; }
mkdir "$WORK/consumer"
cp examples/embedded-application/{package.json,verify.mjs,verify-negative.mjs,scope.mjs} "$WORK/consumer/"
(cd "$WORK/consumer" && npm install --ignore-scripts --no-audit --no-fund "$TARBALL")
python3 -m unittest discover -s examples/embedded-application -p 'test_*.py'
python3 -m unittest discover -s examples/application-notifications -p 'test_*.py'
OXSUM_EMBEDDED_VERIFIER="$WORK/consumer" cargo nextest run -p oxsum-server --test embedded_application --success-output immediate
