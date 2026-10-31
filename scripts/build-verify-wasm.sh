#!/usr/bin/env bash
# Builds the /verify page's in-browser verifier (issue #284): `oxsum_verify`
# compiled for wasm32 and post-processed by wasm-bindgen into the ES module
# the page imports.
#
# Output lands in crates/web/verify-wasm/ over the committed placeholders —
# pages_misc.rs embeds those paths into the binary, so this must run before a
# release build that wants the real verifier. The Dockerfile runs it in the
# `wasm` stage; a plain `cargo build` without it still works, and the page
# then keeps its server-side form (the placeholder glue throws on import).
#
# Needs: rustup's wasm32-unknown-unknown target, and a wasm-bindgen CLI
# matching the crate's wasm-bindgen version (Cargo.lock) — the generated
# module embeds a schema version the CLI validates. Point WASM_BINDGEN at the
# binary when it is not on PATH.

set -euo pipefail

cd "$(dirname "$0")/.."

WASM_BINDGEN="${WASM_BINDGEN:-wasm-bindgen}"

cargo build -p oxsum-verify --release --features wasm \
    --target wasm32-unknown-unknown

# The target dir may be redirected (CARGO_TARGET_DIR, .cargo/config.toml);
# cargo metadata answers it without a build. The artifact is the cdylib at
# <target>/wasm32-unknown-unknown/release/oxsum_verify.wasm.
TARGET_DIR="$(cargo metadata --format-version 1 --no-deps \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"

WASM="$TARGET_DIR/wasm32-unknown-unknown/release/oxsum_verify.wasm"
if [ ! -f "$WASM" ]; then
    echo "expected artifact not found: $WASM" >&2
    exit 1
fi

mkdir -p crates/web/verify-wasm
"$WASM_BINDGEN" --target web --out-name verify_wasm \
    --out-dir crates/web/verify-wasm \
    "$WASM"

echo "verify wasm bundle written to crates/web/verify-wasm/"

# The verify-oxsum-bill npm package ships the same module inside `wasm/` —
# `files` in web/verify-sdk/package.json already declares it, so a package
# build packs whatever this step last synced. The two consumers want the
# identical artifact: one wasm-bindgen run, copied, not a second invocation
# that could drift a wasm-bindgen version away from the page's.
SDK_WASM=web/verify-sdk/wasm
if [ -d "$(dirname "$SDK_WASM")" ]; then
    mkdir -p "$SDK_WASM"
    cp crates/web/verify-wasm/verify_wasm.js \
       crates/web/verify-wasm/verify_wasm_bg.wasm \
       "$SDK_WASM/"
    echo "synced to $SDK_WASM/"
fi
