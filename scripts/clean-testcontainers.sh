#!/usr/bin/env bash
# Sweep orphaned testcontainers containers.
#
# The doubleentry Postgres suite starts a throwaway postgres:17-alpine container
# when no DATABASE_URL/DOUBLEENTRY_DATABASE_URL is set, and relies on
# testcontainers' Ryuk sidecar to remove it when the test process exits. Ryuk's
# sweep is best-effort — a SIGKILLed test binary, an interrupted CI step, or a
# Ryuk that never came up leaves the container (and its anonymous volume)
# behind. This script is the external fix, the same shape
# scripts/drop-test-scratch.sql is for databases: run it before the suites in
# CI, and by hand locally whenever `docker ps -a` shows strays (issue #277).
#
# Safety: only containers carrying the `org.testcontainers.managed-by` label are
# touched, and only once they are older than MIN_AGE_MINUTES (default 60).
# A container younger than that might belong to a suite that is still running —
# deleting it would race the run rather than clean up after it.
#
# Usage:
#   ./scripts/clean-testcontainers.sh            # dry-run: list what would go
#   ./scripts/clean-testcontainers.sh --drop     # actually remove them
#   MIN_AGE_MINUTES=5 ./scripts/clean-testcontainers.sh --drop

set -euo pipefail

MIN_AGE_MINUTES="${MIN_AGE_MINUTES:-60}"
LABEL="org.testcontainers.managed-by=testcontainers"

if [[ "${1:-}" == "--drop" ]]; then
    DRY_RUN=0
elif [[ "${1:-}" == "" ]]; then
    DRY_RUN=1
else
    echo "usage: $0 [--drop]" >&2
    exit 2
fi

if ! command -v docker >/dev/null 2>&1; then
    echo "docker not found; skipping testcontainers sweep" >&2
    exit 0
fi

cutoff_epoch=$(( $(date +%s) - MIN_AGE_MINUTES * 60 ))

# `docker ps -a --filter label=...` does the label match; the age check needs
# each container's CreatedAt, so iterate rather than trusting --filter alone.
stale_ids=()
while IFS= read -r id; do
    created_at=$(docker inspect --format '{{.Created}}' "$id" 2>/dev/null || continue)
    # RFC3339 with nanoseconds — strip the fraction for `date -d`.
    created_epoch=$(date -d "${created_at%.*}" +%s 2>/dev/null || continue)
    if [[ "$created_epoch" -lt "$cutoff_epoch" ]]; then
        stale_ids+=("$id")
    fi
done < <(docker ps -aq --filter "label=$LABEL")

if [[ ${#stale_ids[@]} -eq 0 ]]; then
    echo "no stale testcontainers containers (older than ${MIN_AGE_MINUTES}m)"
    exit 0
fi

for id in "${stale_ids[@]}"; do
    name=$(docker inspect --format '{{.Name}}' "$id" | sed 's|^/||')
    created=$(docker inspect --format '{{.Created}}' "$id")
    printf '%s  %s  created %s\n' "${id:0:12}" "$name" "$created"
done

if [[ "$DRY_RUN" == "1" ]]; then
    echo "dry run — pass --drop to remove"
    exit 0
fi

docker rm -f "${stale_ids[@]}" >/dev/null
# Anonymous volumes left by the removed containers.
docker volume prune -f >/dev/null
echo "removed ${#stale_ids[@]} stale container(s)"
