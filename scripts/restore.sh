#!/usr/bin/env bash
# Compatibility entry point. Requires a private version-2 manifest and fresh
# target; source/cluster selection is explicit and migrations remain separate.
set -euo pipefail
if [ "$#" -ne 2 ]; then
    echo "usage: restore.sh <backup.manifest.json> <fresh-target-database>" >&2
    exit 2
fi
exec python3 "$(dirname "$0")/backup_restore.py" restore "$1" "$2"
