#!/usr/bin/env bash
# Lint a deployment .env for the two mistakes that matter before first boot
# (issue #619): placeholder values left where a secret belongs, and a file any
# local user can read.
#
# Usage: scripts/check-secrets.sh [path-to-.env]   (default: ./.env)
#
# Exits 0 when the file passes, 1 with every problem named — a run before
# `docker compose up` is cheaper than a leaked credential or a boot refused
# on a placeholder.

set -euo pipefail

ENV_FILE="${1:-.env}"

if [ ! -f "$ENV_FILE" ]; then
    echo "check-secrets: $ENV_FILE does not exist" >&2
    exit 1
fi

failed=0
fail() { echo "FAIL  $1"; failed=1; }
ok()   { echo "ok    $1"; }

# --- permissions ---------------------------------------------------------
# The file holds live credentials; anything wider than owner-read is a leak
# waiting for a shared host. Skip on filesystems that cannot express modes.
mode="$(stat -c '%a' "$ENV_FILE" 2>/dev/null || stat -f '%Lp' "$ENV_FILE" 2>/dev/null || echo "")"
if [ -n "$mode" ]; then
    # Accept owner-only files (any owner bits, nothing for group or other):
    # 600, 400, 700. The last two octal digits must be zero.
    case "$mode" in
        *00)
            ok "$ENV_FILE permissions are $mode" ;;
        *)
            fail "$ENV_FILE is mode $mode — secrets files want 600 (chmod 600 $ENV_FILE)" ;;
    esac
fi

# --- placeholders ---------------------------------------------------------
# Values that are syntactically set but obviously not real: the examples
# .env.example ships, the usual "changeme" family, empty-but-uncommented
# secrets. Empty is checked separately — BSD grep refuses empty alternation.
placeholder_re='^(sk-\.\.\.|whsec_\.\.\.|changeme|change-me|change_me|todo|xxx+|placeholder|your-.*|generate-.*|openssl rand.*|<.*>)$'

# Secret-bearing variables a deployment fills in itself. Presence with a
# placeholder value is the failure — absence is fine (the server treats unset
# as "not configured").
secret_vars=(
    OXSUM_SECRET_KEY
    OXSUM_ADMIN_TOKEN
    OXSUM_HEAD_SIGNING_KEY
    OXSUM_WITNESS_SIGNING_KEY
    OXSUM_UPSTREAM_API_KEY
    OXSUM_SMTP_URL
    OXSUM_GITHUB_CLIENT_SECRET
    OXSUM_TURNSTILE_SECRET_KEY
    OXSUM_STRIPE_SECRET_KEY
    OXSUM_STRIPE_WEBHOOK_SECRET
    OXSUM_AIRWALLEX_API_KEY
    OXSUM_AIRWALLEX_WEBHOOK_SECRET
    OXSUM_HYPERSWITCH_API_KEY
    OXSUM_HYPERSWITCH_WEBHOOK_SECRET
    OXSUM_X402_VERIFYING_KEY
    POSTGRES_PASSWORD
)

while IFS= read -r line; do
    # Only KEY=value lines, skipping comments — `export KEY=` spells it too.
    case "$line" in
        ''|'#'*) continue ;;
    esac
    line="${line#export }"
    line="${line#export	}"
    name="${line%%=*}"
    value="${line#*=}"
    # Trim surrounding whitespace and matching quotes.
    name="$(printf '%s' "$name" | tr -d '[:space:]')"
    value="$(printf '%s' "$value" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's/^"\(.*\)"$/\1/' -e "s/^'\(.*\)'\$/\1/")"
    is_secret=0
    for s in "${secret_vars[@]}"; do
        if [ "$name" = "$s" ]; then is_secret=1; break; fi
    done
    [ "$is_secret" -eq 0 ] && continue
    lowered="$(printf '%s' "$value" | tr 'A-Z' 'a-z')"
    if printf '%s' "$lowered" | grep -qE "$placeholder_re"; then
        fail "$name still holds a placeholder value"
    elif [ -z "$value" ]; then
        fail "$name is set but empty — set a real value or comment the line out"
    else
        ok "$name is set"
    fi
done < "$ENV_FILE"

# --- required set ----------------------------------------------------------
# DATABASE_URL (or the POSTGRES_* trio the compose files build it from) is the
# one variable no deployment can boot without.
if ! grep -qE '^[[:space:]]*(export[[:space:]]+)?DATABASE_URL=' "$ENV_FILE" \
   && ! grep -qE '^[[:space:]]*(export[[:space:]]+)?POSTGRES_PASSWORD=' "$ENV_FILE"; then
    fail "neither DATABASE_URL nor POSTGRES_PASSWORD is set — the deployment has no database"
fi

if [ "$failed" -ne 0 ]; then
    echo "check-secrets: $ENV_FILE has problems" >&2
    exit 1
fi
echo "check-secrets: $ENV_FILE is clean"
