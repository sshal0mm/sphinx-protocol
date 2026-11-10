# One image per role, built from this repository on the server.
#   docker compose -f deploy/compose.yml build
FROM rust:1.99-bookworm AS rust
WORKDIR /src
COPY rust-toolchain.toml Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p sphinx-api -p sphinx-relayer -p sphinx-house -p sphinx-mcp \
 && mkdir -p /out && cp target/release/sphinx-api target/release/sphinx-relayer \
      target/release/sphinx-house target/release/sphinx-mcp /out/

FROM debian:bookworm-slim AS services
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates \
 && rm -rf /var/lib/apt/lists/* && useradd -r -u 10001 -m sphinx
COPY --from=rust /out/ /usr/local/bin/
USER sphinx
WORKDIR /home/sphinx

# The bare site: one static page and the docs, with the API paths passed on.
FROM nginx:1.27.3-alpine AS site
COPY deploy/site.conf /etc/nginx/conf.d/default.conf
COPY site /usr/share/nginx/html
COPY docs/guide /usr/share/nginx/html/docs/guide
