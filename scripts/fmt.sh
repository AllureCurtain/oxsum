#!/usr/bin/env bash
# Formats the workspace's own crates with the pinned nightly rustfmt.
#
# Usage:
#   scripts/fmt.sh          rewrite formatting in place
#   scripts/fmt.sh --check  fail when a file would change
#
# The toolchain version lives in `rustfmt-nightly` at the repository root —
# the same file CI reads — so a local run and a CI run cannot use different
# formatters. Never use bare `cargo fmt`: stable silently ignores the unstable
# options this repository needs, and `cargo fmt --all` would reorder the
# vendored `crates/doubleentry`.

set -euo pipefail

cd "$(dirname "$0")/.."

VERSION="$(tr -d '[:space:]' < rustfmt-nightly)"
if [ -z "$VERSION" ]; then
    echo "rustfmt-nightly is empty; it must name a nightly toolchain" >&2
    exit 1
fi

if ! rustup run "$VERSION" rustfmt --version >/dev/null 2>&1; then
    echo "rustfmt for $VERSION is not installed. Install it with:" >&2
    echo "  rustup toolchain install $VERSION --component rustfmt" >&2
    exit 1
fi

# The workspace's own crates. `crates/doubleentry` is vendored upstream code
# and is deliberately absent: keeping upstream's layout is what makes future
# merges cheap.
CRATES=(
    -p oxsum-core
    -p oxsum-server
    -p oxsum-integrations
    -p oxsum-verify
    -p oxsum-web
    -p oxsum-testkit
)

cargo "+$VERSION" fmt "${CRATES[@]}" "$@"
