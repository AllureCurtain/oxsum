# oxsum release image: one binary against an external PostgreSQL supplied
# through DATABASE_URL at run time.
#
# Build:  docker build -t oxsum .
# Run:    docker run --rm -p 3000:3000 -e DATABASE_URL=postgresql://user:pass@host/oxsum oxsum
#
# The builder matches rust-toolchain.toml (Rust 1.98). The Topcoat dashboard
# is server-side rendered; the React scaffold (issue #234) builds in the
# `frontend` stage and is embedded via rust-embed — the runtime is still the
# single binary, reqwest uses rustls, so the only system package it needs is
# the CA bundle.

# ---------- frontend ----------
# The React dashboard (issue #234): Vite + React, built to web/dist and
# embedded into the binary via rust-embed. The node stage runs codegen from
# crates/server/openapi.yaml so the typed client can never drift the contract.
FROM node:22-alpine AS frontend

WORKDIR /web
COPY web/package.json web/package-lock.json ./
RUN npm ci
COPY web/ ./
COPY crates/server/openapi.yaml /crates/server/openapi.yaml
RUN npm run codegen && npm run build

# ---------- verify wasm ----------
# The /verify page's in-browser verifier (issue #284): oxsum_verify compiled
# for wasm32 and post-processed by wasm-bindgen into the ES module the page
# imports, written to crates/web/verify-wasm/ where pages_misc.rs embeds it.
# The CLI is the prebuilt release binary whose version must equal the
# wasm-bindgen crate's (Cargo.lock) — the generated module embeds a schema
# version the CLI validates.
FROM rust:1.98-bookworm AS wasm

ARG WASM_BINDGEN_VERSION=0.2.129
RUN rustup target add wasm32-unknown-unknown \
    && curl -fsSL \
        "https://github.com/rustwasm/wasm-bindgen/releases/download/${WASM_BINDGEN_VERSION}/wasm-bindgen-${WASM_BINDGEN_VERSION}-x86_64-unknown-linux-musl.tar.gz" \
        | tar -xz --strip-components=1 \
            -C /usr/local/bin \
            "wasm-bindgen-${WASM_BINDGEN_VERSION}-x86_64-unknown-linux-musl/wasm-bindgen"

WORKDIR /app
# .cargo/config.toml is deliberately not copied: it pins mold for Linux
# links, and the host-side proc-macro builds would fail without mold
# installed — the wasm bundle links nothing. Its release profile only costs
# the bundle a little size, so the stage builds stock.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
# cargo resolves the workspace before building -p oxsum-verify, so every
# member's manifest must exist; only doubleentry + verify carry real sources.
COPY crates/doubleentry ./crates/doubleentry
COPY crates/verify ./crates/verify
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/web/Cargo.toml crates/web/Cargo.toml
COPY crates/integrations/Cargo.toml crates/integrations/Cargo.toml
COPY crates/testkit/Cargo.toml crates/testkit/Cargo.toml
RUN mkdir -p crates/core/src crates/server/src crates/web/src \
        crates/integrations/src crates/testkit/src \
    && for c in core web integrations testkit; do echo "" > crates/$c/src/lib.rs; done \
    && echo "" > crates/server/src/lib.rs \
    && echo 'fn main() {}' > crates/server/src/main.rs
COPY scripts/build-verify-wasm.sh scripts/build-verify-wasm.sh
RUN bash scripts/build-verify-wasm.sh

# ---------- builder ----------
FROM rust:1.98-bookworm AS builder

# .cargo/config.toml forces mold as the linker on Linux; without it the
# release link fails with "cannot find 'ld'".
RUN apt-get update \
    && apt-get install -y --no-install-recommends mold \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /app

# Dependency layer: manifests (and the cargo config that pins the toolchain and
# the release profile) first, stub sources to satisfy cargo's workspace layout.
# A later source edit only invalidates the layers below, not this one.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY .cargo/config.toml .cargo/config.toml
COPY crates/doubleentry/Cargo.toml crates/doubleentry/Cargo.toml
COPY crates/core/Cargo.toml crates/core/Cargo.toml
COPY crates/verify/Cargo.toml crates/verify/Cargo.toml
COPY crates/server/Cargo.toml crates/server/Cargo.toml
COPY crates/web/Cargo.toml crates/web/Cargo.toml
COPY crates/integrations/Cargo.toml crates/integrations/Cargo.toml
COPY crates/testkit/Cargo.toml crates/testkit/Cargo.toml
RUN mkdir -p crates/doubleentry/src crates/core/src crates/verify/src \
        crates/server/src crates/web/src crates/integrations/src crates/testkit/src \
    && for c in doubleentry core verify web integrations testkit; do echo "" > crates/$c/src/lib.rs; done \
    && echo "" > crates/server/src/lib.rs \
    && echo 'fn main() {}' > crates/server/src/main.rs \
    && cargo build --release --workspace \
    && rm -rf target

# Real sources. Only this layer and below rebuild when .rs files change.
COPY . .
# The built React app lands over the committed placeholder so rust-embed
# embeds the real bundle; the verify wasm artifacts land over the committed
# placeholders pages_misc.rs embeds the same way.
COPY --from=frontend /web/dist ./web/dist
COPY --from=wasm /app/crates/web/verify-wasm ./crates/web/verify-wasm
# The server binary lands in target/release/<bin-exe-name> (here: oxsum).
RUN cargo build --release \
    && test -x /app/target/release/oxsum

# ---------- runtime ----------
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/oxsum /usr/local/bin/oxsum

# Inside a container the server must listen on all interfaces.
# DATABASE_URL (external PostgreSQL) is supplied at run time; the schema is
# migrated at startup, so no migrate step is needed.
ENV OXSUM_ADDR=0.0.0.0:3000 \
    RUST_LOG=info

EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/oxsum"]
