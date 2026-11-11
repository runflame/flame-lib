# The overlay as an image. Build context is the repository root:
#   docker build -f btc-overlay/Dockerfile -t btc-overlay .
# Dockerfile.dockerignore next to this file keeps target/ and .git out of the
# context; podman does not read it, so pass it there:
#   podman build --ignorefile btc-overlay/Dockerfile.dockerignore ...
# CI builds and pushes it as ghcr.io/runflame/btc-overlay:<commit> and `:main`
# (.github/workflows/btc-overlay-image.yml).
FROM docker.io/library/rust:1.90-bookworm AS build

# The image already ships 1.90.0. Naming it here overrides
# rust-toolchain.toml, whose clippy/rustfmt/rust-analyzer components a
# release build does not need and rustup would otherwise download.
ENV RUSTUP_TOOLCHAIN=1.90.0

WORKDIR /src
COPY . .

RUN cargo build --release --locked -p btc-overlay

FROM docker.io/library/debian:bookworm-slim

LABEL org.opencontainers.image.source=https://github.com/runflame/flame-lib

RUN groupadd --system --gid 10003 btc-overlay \
    && useradd --system --uid 10003 --gid 10003 --home-dir /var/lib/btc-overlay btc-overlay \
    && mkdir -p /var/lib/btc-overlay \
    && chown btc-overlay:btc-overlay /var/lib/btc-overlay

COPY --from=build /src/target/release/btc-overlay /usr/local/bin/

USER btc-overlay
WORKDIR /var/lib/btc-overlay
# overlay.log, every block's contents. Losing it changes what the nodes were
# served: a new overlay, and the nodes' chains with it.
VOLUME /var/lib/btc-overlay
EXPOSE 18443

# The Blockbook URL is the deployment's (--blockbook), so it has no default.
ENTRYPOINT ["/usr/local/bin/btc-overlay", "--log", "/var/lib/btc-overlay/overlay.log", "--listen", "0.0.0.0:18443"]
