#!/usr/bin/env bash
# Compatibility entry point. Select CONTAINER+POSTGRES_DB or DATABASE_URL
# explicitly; this wrapper never sources .env or discovers a Docker container.
set -euo pipefail
if [ "$#" -gt 1 ]; then
    echo "usage: backup.sh [output-directory]" >&2
    exit 2
fi
exec python3 "$(dirname "$0")/backup_restore.py" backup "${1:-.}"
