#!/usr/bin/env bash
# Validate a deployment .env against .env.example before `docker compose up`
# (issue #619): every variable the example marks required-for-this-deployment
# must be present in .env, and no .env line may name a variable the server
# does not know — a typo'd key is a silent misconfiguration, exactly the
# failure this check exists to catch.
#
# Usage: scripts/check-config.sh [path-to-.env] [path-to-.env.example]
#   defaults: ./.env and ./.env.example
#
# Exits 0 on a clean file, 1 with every problem named.

set -euo pipefail

ENV_FILE="${1:-.env}"
EXAMPLE="${2:-.env.example}"

for f in "$ENV_FILE" "$EXAMPLE"; do
    if [ ! -f "$f" ]; then
        echo "check-config: $f does not exist" >&2
        exit 1
    fi
done

failed=0
fail() { echo "FAIL  $1"; failed=1; }
ok()   { echo "ok    $1"; }

# The variable names .env.example knows: uncommented assignments plus the
# documented-but-commented ones (`# OXSUM_FOO=`). A name absent from both
# lists is a typo the server would ignore.
known_names() {
    grep -oE '^[[:space:]#]*[A-Z_][A-Z0-9_]*=' "$EXAMPLE" \
        | tr -d '# ' | tr -d '=' | sort -u
    # operator.env.example's compose-side variables are legitimate too when
    # the file being checked is a deploy .env.
    printf '%s\n' POSTGRES_USER POSTGRES_PASSWORD POSTGRES_DB \
        OXSUM_IMAGE_TAG OXSUM_BIND OXSUM_PORT OXSUM_RAILS \
        OXSUM_BUSINESS_NAME OXSUM_BUSINESS_SLUG OXSUM_CUSTOMER_SELF_REGISTER \
        OXSUM_DEMO_SEED DOUBLEENTRY_DATABASE_URL
}

KNOWN="$(known_names)"

# Lines that are set (not commented) in .env — an `export KEY=` spelling
# counts the same as a bare `KEY=`.
set_names() {
    grep -vE '^[[:space:]]*(#|$)' "$ENV_FILE" \
        | sed -E 's/^[[:space:]]*export[[:space:]]+//' \
        | grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' | tr -d '=' | sort -u
}

unknown=0
while IFS= read -r name; do
    [ -z "$name" ] && continue
    if ! printf '%s\n' "$KNOWN" | grep -qx "$name"; then
        fail "$name is set in $ENV_FILE but no example documents it — typo, or a removed variable"
        unknown=1
    fi
done < <(set_names)
[ "$unknown" -eq 0 ] && ok "every variable in $ENV_FILE is a documented name"

# The variables a boot cannot proceed without. DATABASE_URL itself is
# compose-built from POSTGRES_* on the deploy profiles, so either spells it.
if ! grep -qE '^[[:space:]]*(export[[:space:]]+)?DATABASE_URL=' "$ENV_FILE" \
   && ! { grep -qE '^[[:space:]]*(export[[:space:]]+)?POSTGRES_PASSWORD=' "$ENV_FILE"; }; then
    fail "no DATABASE_URL and no POSTGRES_PASSWORD: the deployment has no database to point at"
else
    ok "a database is configured"
fi

# Obvious contradictions the server also refuses — checked here so the message
# names the file, not just the variable.
val() { grep -oE "^[[:space:]]*(export[[:space:]]+)?$1=.*" "$ENV_FILE" | tail -n1 | cut -d= -f2- | tr -d '"'"'"'[:space:]' || true; }

demo="$(val OXSUM_DEMO_SEED)"
case "$demo" in
    true|1)
        echo "warn  OXSUM_DEMO_SEED=$demo — demo data is opt-in; unset it on a deployment you mean to run"
        ;;
esac

if [ "$failed" -ne 0 ]; then
    echo "check-config: $ENV_FILE has problems" >&2
    exit 1
fi
echo "check-config: $ENV_FILE is complete against $EXAMPLE"
