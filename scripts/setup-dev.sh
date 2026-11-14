#!/usr/bin/env bash
# One-time setup for a fresh clone (issue #1215): makes a local commit run the
# same formatter and checks the CI fast tier runs, so "green locally, red in
# CI" formatting failures stop reaching PRs.
#
#   1. installs the nightly rustfmt pinned in `rustfmt-nightly` (the version
#      `scripts/fmt.sh`, the CI `fmt` job and the pre-commit hook all read)
#   2. points git at the versioned hooks in `.githooks/`
#   3. runs `scripts/fmt-check.sh` once so a mismatch shows up now
#
# Idempotent; safe to re-run after `rustfmt-nightly` is bumped.
#
# Usage: scripts/setup-dev.sh

set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="$(tr -d '[:space:]' < rustfmt-nightly)"
if ! command -v rustup >/dev/null 2>&1; then
    echo "rustup is required: https://rustup.rs" >&2
    exit 1
fi

if rustup run "$VERSION" rustfmt --version >/dev/null 2>&1; then
    echo "setup-dev: rustfmt $VERSION already installed"
else
    echo "setup-dev: installing rustfmt $VERSION"
    rustup toolchain install "$VERSION" --profile minimal --component rustfmt
fi

current="$(git config --get core.hooksPath || true)"
if [ "$current" != ".githooks" ]; then
    if [ -n "$current" ]; then
        echo "setup-dev: replacing core.hooksPath=$current with .githooks"
    fi
    git config core.hooksPath .githooks
fi
echo "setup-dev: git hooks -> .githooks (bypass once with 'git commit --no-verify')"

if scripts/fmt-check.sh >/dev/null 2>&1; then
    echo "setup-dev: formatting matches CI"
else
    echo "setup-dev: formatting differs from CI; run scripts/fmt.sh" >&2
fi
