#!/usr/bin/env bash
# Run every maintained example against resources owned by this invocation.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
exec "${PYTHON:-python3}" "$ROOT/scripts/verify-examples.py" "$@"
