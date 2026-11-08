#!/usr/bin/env bash
# Same-cluster drill through the verified production pair. Explicit inputs only;
# the restored database is retained and any pre-existing target is refused.
set -euo pipefail
: "${SOURCE:?set SOURCE to the source database}"
: "${TARGET:?set TARGET to a fresh restore database}"
: "${OUT_DIR:?set OUT_DIR to the private backup directory}"
if [ "$SOURCE" = "$TARGET" ]; then
    echo "SOURCE and TARGET must differ" >&2
    exit 2
fi
export POSTGRES_DB="$SOURCE"
exec python3 "$(dirname "$0")/backup_restore.py" drill "$OUT_DIR" "$TARGET"
