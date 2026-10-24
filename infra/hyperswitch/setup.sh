#!/usr/bin/env bash
# Clone the hyperswitch repo into infra/hyperswitch/.repo — the migrations and
# the justfile the migration_runner mounts live there, not in this directory.
# The clone is a cache; delete it freely and re-run this script.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$HERE/.repo"
REF="${1:-main}"

if [ -d "$REPO_DIR/.git" ]; then
    echo "infra/hyperswitch/.repo already exists; pull it or delete and re-run."
    exit 0
fi

git clone --depth 1 --branch "$REF" https://github.com/juspay/hyperswitch.git "$REPO_DIR"

# Line endings: the scripts mount into Linux containers, so LF is required.
# Some checkouts on Windows produce CRLF; normalize the mounted set.
find "$REPO_DIR/scripts" "$REPO_DIR/migrations" "$REPO_DIR/config" -type f \
    \( -name '*.sh' -o -name '*.sql' -o -name '*.toml' \) \
    -exec sed -i 's/\r$//' {} + 2>/dev/null || true

# Upstream's drop_id.patch does not apply to the schema this branch generates
# (hunk #5 fails) and print-schema is a codegen aid the runtime does not need.
sed -i 's/^patch_file="crates\/diesel_models\/drop_id.patch"$/# patch_file disabled by oxsum setup.sh — see infra\/hyperswitch\/README.md/' \
    "$REPO_DIR/diesel.toml"

echo "hyperswitch repo at $REPO_DIR (ref $REF)"
