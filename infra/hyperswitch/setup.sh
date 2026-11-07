#!/usr/bin/env bash
# Prepare the exact pinned source/tools and private sandbox configuration.
set -euo pipefail
exec python3 "$(dirname "${BASH_SOURCE[0]}")/stack.py" prepare "$@"
