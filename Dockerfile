# oxsum release image: one binary plus the Leptos dashboard site, against an
# external PostgreSQL supplied through DATABASE_URL at run time.
#
# Build:  docker build -t oxsum .
# Run:    docker run --rm -p 3000:3000 -e DATABASE_URL=postgresql://user:pass@host/oxsum oxsum
#
# The builder matches rust-toolchain.toml (Rust 1.98). cargo-leptos 0.3.11 builds the
# server binary and the WASM browser side together; the dashboard's style is plain CSS,
# so no Node.js or npm is needed. The runtime is slim: reqwest uses rustls, so the
# only system package it needs is the CA bundle.

# ---------- builder ----------
FROM rust:1.98-bookworm AS builder

WORKDIR /app

# The Leptos browser side compiles to WASM; cargo-leptos drives that build.
RUN rustup target add wasm32-unknown-unknown \
    && cargo install cargo-leptos --version 0.3.11

COPY . .
# The server binary lands in target/release/<bin-exe-name> (here: oxsum); the site
# (WASM bundle, CSS, index.html) lands in target/site.
RUN cargo leptos build --release \
    && test -x /app/target/release/oxsum \
    && test -d /app/target/site/pkg

# ---------- runtime ----------
FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/oxsum /usr/local/bin/oxsum
COPY --from=builder /app/target/site /app/site

# Inside a container the server must listen on all interfaces; the dashboard reads its
# files from LEPTOS_SITE_ROOT. DATABASE_URL (external PostgreSQL) is supplied at run
# time; the schema is migrated at startup, so no migrate step is needed.
ENV OXSUM_ADDR=0.0.0.0:3000 \
    LEPTOS_SITE_ROOT=/app/site \
    RUST_LOG=info

EXPOSE 3000

ENTRYPOINT ["/usr/local/bin/oxsum"]
