# Multi-stage: build the workspace, then ship a slim image with only the
# server binary plus the PDF files baked in.

FROM rust:1-slim AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock* ./
COPY shared/Cargo.toml shared/Cargo.toml
COPY server/Cargo.toml server/Cargo.toml
COPY load_balancer/Cargo.toml load_balancer/Cargo.toml

# Pre-fetch deps with empty crate stubs so layer caching survives source edits.
RUN mkdir -p shared/src server/src load_balancer/src \
 && echo "" > shared/src/lib.rs \
 && echo "fn main(){}" > server/src/main.rs \
 && echo "fn main(){}" > load_balancer/src/main.rs \
 && cargo build --release -p server || true

COPY shared/src shared/src
COPY server/src server/src
COPY load_balancer/src load_balancer/src

# buildkit's COPY preserves mtimes from the host. The stub `*.rs` files we
# wrote in the previous layer have *newer* timestamps than the real sources
# we just copied in, so cargo's freshness check would think nothing changed
# and we'd ship the empty-main stub binary. Bump the mtimes to force a real
# rebuild.
RUN find shared server load_balancer -name '*.rs' -exec touch {} + \
 && cargo build --release -p server

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/server /app/server
COPY server/src/files /app/files

ENV FILES_DIR=/app/files
EXPOSE 4443
CMD ["/app/server"]
