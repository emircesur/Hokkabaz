# syntax=docker/dockerfile:1
# hokkabazd: headless Hokkabaz instance. Build from the repo root:
#   docker build -t hokkabazd .
# Data lives in the /data volume (db, sessions, secrets.json). Non-root, no shell tooling in
# the runtime image; the healthcheck is the binary itself.

FROM rust:1.88-slim-bookworm AS build
WORKDIR /src
# C toolchain for bundled sqlite / QuickJS builds
RUN apt-get update \
 && apt-get install -y --no-install-recommends build-essential pkg-config ca-certificates \
 && rm -rf /var/lib/apt/lists/*
COPY . .
# --locked: the committed Cargo.lock is the build. Cache mounts keep rebuilds quick.
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked -p hk-daemon \
 && install -D target/release/hokkabazd /out/hokkabazd \
 && mkdir -p /out/data

# cc variant: the binary links glibc and libgcc_s.
FROM gcr.io/distroless/cc-debian12:nonroot
COPY --from=build /out/hokkabazd /usr/local/bin/hokkabazd
ENV HOKKABAZ_HOME=/data \
    HOKKABAZ_LISTEN=0.0.0.0:7878
# Pre-created and owned by nonroot (uid 65532): a fresh named volume inherits this.
COPY --from=build --chown=65532:65532 /out/data /data
VOLUME ["/data"]
WORKDIR /data
EXPOSE 7878
USER nonroot:nonroot
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
  CMD ["/usr/local/bin/hokkabazd", "healthcheck"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/hokkabazd"]
CMD ["serve"]
