FROM rust:1-slim AS builder
WORKDIR /build

COPY Cargo.toml Cargo.lock* ./
COPY shared/Cargo.toml shared/Cargo.toml
COPY server/Cargo.toml server/Cargo.toml
COPY load_balancer/Cargo.toml load_balancer/Cargo.toml

RUN mkdir -p shared/src server/src load_balancer/src \
 && echo "" > shared/src/lib.rs \
 && echo "fn main(){}" > server/src/main.rs \
 && echo "fn main(){}" > load_balancer/src/main.rs \
 && cargo build --release -p load_balancer || true

COPY shared/src shared/src
COPY server/src server/src
COPY load_balancer/src load_balancer/src

# buildkit's COPY preserves mtimes from the host. The stub `*.rs` files we
# wrote in the previous layer have *newer* timestamps than the real sources
# we just copied in, so cargo's freshness check would think nothing changed
# and we'd ship the empty-main stub binary. Bump the mtimes to force a real
# rebuild.
RUN find shared server load_balancer -name '*.rs' -exec touch {} + \
 && cargo build --release -p load_balancer

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/*

COPY --from=builder /build/target/release/load_balancer /app/load_balancer

EXPOSE 8080 8443 9443
CMD ["/app/load_balancer"]
