#!/usr/bin/env bash
# Manual test for .githooks/pre-commit's path→crate mapping.
# Runs the hook with a stubbed `git diff --cached` so no real staging is needed.
#
# Usage: scripts/test-pre-commit.sh

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

# Stub git so the hook sees exactly the staged files we feed it.
# Everything the hook calls is `git diff --cached --name-only` or `git rev-parse`.
GIT_STUB=$(mktemp -d)
cat > "$GIT_STUB/git" << 'STUB'
#!/usr/bin/env bash
case "$*" in
    "diff --cached --name-only --diff-filter=ACMR") cat "$STAGED_FILE" ;;
    "rev-parse --show-toplevel") pwd ;;
    *) exit 0 ;;
esac
STUB
chmod +x "$GIT_STUB/git"
export PATH="$GIT_STUB:$PATH"

HOOK=.githooks/pre-commit
PASS=0; FAIL=0

run_case() {
    local name="$1"; shift
    local files="$1"; shift
    local expected="$1"; shift
    STAGED_FILE=$(mktemp)
    printf '%s\n' "$files" > "$STAGED_FILE"
    export STAGED_FILE
    local out
    if out=$(bash "$HOOK" 2>&1); then
        if echo "$out" | grep -q "$expected"; then
            echo "PASS: $name"
            PASS=$((PASS + 1))
        else
            echo "FAIL: $name — expected '$expected' in:"
            echo "$out" | tail -3
            FAIL=$((FAIL + 1))
        fi
    else
        echo "FAIL: $name — hook exited non-zero"
        echo "$out" | tail -5
        FAIL=$((FAIL + 1))
    fi
    rm -f "$STAGED_FILE"
}

# Docs-only → skip clippy
run_case "docs only" "docs/development.md" "skipping clippy"

# Markdown at root → skip clippy
run_case "markdown only" "TODO.md
AGENTS.md" "skipping clippy"

# Frontend → skip clippy
run_case "web only" "web/src/App.tsx
web/src/index.css" "skipping clippy"

# Scripts → skip clippy
run_case "scripts only" "scripts/x402_sign.py" "skipping clippy"

# Single crate → scoped clippy
run_case "core only" "crates/core/src/wallet.rs" "clippy -p oxsum-core"

# Two crates → two scoped lints
run_case "core + server" "crates/core/src/wallet.rs
crates/server/src/main.rs" "clippy -p oxsum-server"

# Cargo.toml → workspace
run_case "root manifest" "Cargo.toml" "clippy --workspace"

# Cargo.lock → workspace
run_case "lockfile" "Cargo.lock" "clippy --workspace"

# Unknown top-level → workspace (safe default)
run_case "unknown top-level" "newdir/foo.rs" "clippy --workspace"

# Mixed: docs + one crate → scoped, not workspace
run_case "docs + core" "docs/development.md
crates/core/src/seed.rs" "clippy -p oxsum-core"

# Hook itself → skip
run_case "hook edit" ".githooks/pre-commit" "skipping clippy"

echo ""
echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
