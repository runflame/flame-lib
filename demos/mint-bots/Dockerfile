# The mint bots as one image. Build context is the repository root, which
# flamekd is built from:
#   docker build -f demos/mint-bots/Dockerfile -t mint-bots .
# Dockerfile.dockerignore next to this file keeps target/ and .git out of the
# context; podman does not read it, so pass it there:
#   podman build --ignorefile demos/mint-bots/Dockerfile.dockerignore ...
# CI builds and pushes it as ghcr.io/runflame/mint-bots:<commit> and `:main`
# (.github/workflows/mint-bots-image.yml).
FROM docker.io/library/rust:1.90-bookworm AS build

# The image already ships 1.90.0; this overrides rust-toolchain.toml, whose
# extra components a release build does not need.
ENV RUSTUP_TOOLCHAIN=1.90.0

WORKDIR /src
COPY . .
RUN cargo build --release --locked --manifest-path demos/mint-bots/Cargo.toml

FROM docker.io/library/debian:bookworm-slim

LABEL org.opencontainers.image.source=https://github.com/runflame/flame-lib

# Pinned uid and gid: a host directory mounted as the volume needs a known owner.
RUN groupadd --system --gid 10002 bots \
    && useradd --system --uid 10002 --gid 10002 --home-dir /var/lib/mint-bots bots \
    && mkdir -p /var/lib/mint-bots \
    && chown bots:bots /var/lib/mint-bots

COPY --from=build /src/demos/mint-bots/target/release/mint-bots /usr/local/bin/mint-bots

ENV MINT_BOTS_LISTEN=0.0.0.0:3100

# The commit, shown on the control page and in the journal's start events;
# CI passes it. Last, so a new commit rebuilds only this layer.
ARG MINT_REVISION=
ENV MINT_REVISION=$MINT_REVISION

USER bots
WORKDIR /var/lib/mint-bots
# The journal; one container per journal (the service locks it).
VOLUME /var/lib/mint-bots
EXPOSE 3100

ENTRYPOINT ["/usr/local/bin/mint-bots"]
CMD ["/var/lib/mint-bots/bots.sqlite3"]
