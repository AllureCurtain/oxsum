#!/usr/bin/env bash
# Checks formatting with the pinned nightly rustfmt. See scripts/fmt.sh.

set -euo pipefail

cd "$(dirname "$0")/.."

exec scripts/fmt.sh --check "$@"
