# The mint playground as one image: the service and the frontend it serves. Build
# context is the repository root, which flamekd and the payout wallet are built from:
#   docker build -f demos/mint-playground/Dockerfile -t mint-playground .
# Dockerfile.dockerignore next to this file keeps target/, node_modules/ and .git
# out of the context; podman does not read it, so pass it there:
#   podman build --ignorefile demos/mint-playground/Dockerfile.dockerignore ...
# CI builds and pushes it as ghcr.io/runflame/mint-playground:<commit> and
# `:main` (.github/workflows/mint-playground-image.yml).

# The frontend: React, Vite and the browser wallet (@runflame/wallet-wasm) from npm.
# Dependencies are a layer of their own, invalidated by the lockfile alone.
FROM docker.io/library/node:24-bookworm-slim AS web
WORKDIR /web
COPY demos/mint-playground/web/package.json demos/mint-playground/web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY demos/mint-playground/web ./
RUN npm run build

FROM docker.io/library/rust:1.90-bookworm AS build

# The image already ships 1.90.0. Naming it here overrides
# rust-toolchain.toml, whose clippy/rustfmt/rust-analyzer components a
# release build does not need and rustup would otherwise download.
ENV RUSTUP_TOOLCHAIN=1.90.0

WORKDIR /src
COPY . .
RUN cargo build --release --locked --manifest-path demos/mint-playground/Cargo.toml

FROM docker.io/library/debian:bookworm-slim

LABEL org.opencontainers.image.source=https://github.com/runflame/flame-lib

# The gid is pinned too: `useradd --system` alone would give the group a
# system gid, and a host directory mounted as the volume needs a known owner.
RUN groupadd --system --gid 10001 mint \
    && useradd --system --uid 10001 --gid 10001 --home-dir /var/lib/mint-playground mint \
    && mkdir -p /var/lib/mint-playground \
    && chown mint:mint /var/lib/mint-playground

COPY --from=build /src/demos/mint-playground/target/release/mint-playground /usr/local/bin/mint-playground
COPY --from=web /web/dist /usr/share/mint-playground/web

ENV MINT_LISTEN=0.0.0.0:3000
ENV MINT_WEB_DIR=/usr/share/mint-playground/web

# The commit, sent as the X-Mint-Revision response header; CI passes it.
# Last, so a new commit rebuilds only this layer of the final stage.
ARG MINT_REVISION=
ENV MINT_REVISION=$MINT_REVISION

USER mint
WORKDIR /var/lib/mint-playground
# The SQLite ledger; one container per database (the service locks it).
VOLUME /var/lib/mint-playground
EXPOSE 3000

# The service shuts down cleanly on SIGINT only; SIGTERM would wait out the
# stop timeout and end in SIGKILL.
STOPSIGNAL SIGINT

ENTRYPOINT ["/usr/local/bin/mint-playground"]
CMD ["/var/lib/mint-playground/mint.sqlite3"]
