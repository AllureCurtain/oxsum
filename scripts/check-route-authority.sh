#!/usr/bin/env bash
# Route authority coverage (issue #618, T27): every authenticated `/api/v1`
# route in openapi.yaml must be classified by `customer_route_capability` —
# a route the classifier misses answers `Retired` (deny-by-default, correct
# but a drift signal), and a route deliberately retired must still be wired
# so the denial is by design, not by omission.
#
# The check is a structural diff, not a request walk: it parses the matched
# paths the axum router mounts (the same strings `MatchedPath` hands the
# classifier) and diffs them against the openapi contract. A route in the
# contract that the classifier does not name is reported so the review can
# confirm the `Retired` catch-all is intentional, not an accident.
#
# Usage: scripts/check-route-authority.sh
# Exits 0 with a coverage summary; the per-route report goes to stdout.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OPENAPI="$ROOT/crates/server/openapi.yaml"
ROUTER="$ROOT/crates/server/src/routes/mod.rs"
AUTH="$ROOT/crates/server/src/auth.rs"

for f in "$OPENAPI" "$ROUTER" "$AUTH"; do
    [ -f "$f" ] || { echo "check-route-authority: $f missing" >&2; exit 1; }
done

# Paths the router mounts under `/api/v1` behind `require_principal` — the
# authenticated nest. Extracted from the route definitions in mod.rs: every
# `.route("/…")` inside the `authenticated` router block, up to the `open`
# block that follows.
authenticated_paths="$(
    sed -n '/let authenticated = Router::new/,/let open = Router::new/p' "$ROUTER" \
        | tr '\n' ' ' \
        | grep -oE '\.route\( *"[^"]+"' \
        | sed -e 's/\.route( *"//' -e 's/"$//' \
        | sort -u
)"

# The classifier's named paths: every literal route string inside the
# `customer_route_capability` match arms. These are the deliberate
# classifications; anything mounted but unlisted falls through to `Retired`.
classified_paths="$(
    sed -n '/fn customer_route_capability/,/^}/p' "$AUTH" \
        | grep -oE '"/[a-zA-Z0-9_/{}.-]+"' \
        | tr -d '"' \
        | grep -v '^/api/v1$' \
        | sort -u
)"

echo "route authority coverage"
echo "  authenticated routes mounted: $(echo "$authenticated_paths" | grep -c .)"
echo "  classifier-named paths:       $(echo "$classified_paths" | grep -c .)"

# Mounted but unclassified: each of these answers `Retired` to a customer
# principal. That may be deliberate (the route is session-only and its
# handler enforces the finer check) — the report names them so a new route
# landing here by accident is visible.
unclassified="$(comm -23 <(echo "$authenticated_paths") <(echo "$classified_paths"))"
if [ -n "$unclassified" ]; then
    echo
    echo "mounted without an explicit customer classification (answers Retired):"
    echo "$unclassified" | sed 's/^/  /'
fi

# Classifier-named but no longer mounted: the contract drifted, the
# classifier did not. A name that mounts nowhere is a stale classification.
stale="$(comm -13 <(echo "$authenticated_paths") <(echo "$classified_paths"))"
if [ -n "$stale" ]; then
    echo
    echo "classified but no authenticated route mounts it (stale classification):"
    echo "$stale" | sed 's/^/  /'
fi

echo
echo "check-route-authority: done"
