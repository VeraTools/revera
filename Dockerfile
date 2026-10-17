FROM debian:trixie-slim AS binaries

ARG TARGETARCH
ARG DEBIAN_FRONTEND=noninteractive
ARG VERA_VERSION=2.1.4

RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl python3 \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /build
COPY revera-*-unknown-linux-musl.tar.gz* ./

# Both tools come from release archives; no compilation happens in this image.
RUN set -eu; \
    case "$TARGETARCH" in \
      amd64) arch=x86_64 ;; \
      arm64) arch=aarch64 ;; \
      *) echo "unsupported architecture: $TARGETARCH" >&2; exit 1 ;; \
    esac; \
    archive="revera-${arch}-unknown-linux-musl.tar.gz"; \
    sha256sum -c "$archive.sha256"; \
    mkdir -p /out /build/revera /build/vera; \
    tar -xzf "$archive" -C /build/revera revera; \
    install -m755 /build/revera/revera /out/revera; \
    target="${arch}-unknown-linux-gnu"; \
    archive="vera-${target}.tar.gz"; \
    base="https://github.com/VeraTools/vera/releases/download/v${VERA_VERSION}"; \
    curl -fsSL "$base/$archive" -o "$archive"; \
    curl -fsSL "$base/release-manifest.json" -o manifest.json; \
    want=$(python3 -c 'import json, sys; m = json.load(open(sys.argv[1])); assets = m.get("assets", m); entry = assets[sys.argv[2]] if isinstance(assets, dict) else next(a for a in assets if sys.argv[2] in json.dumps(a)); print(entry["sha256"])' manifest.json "$target"); \
    test -n "$want"; \
    printf '%s  %s\n' "$want" "$archive" | sha256sum -c -; \
    tar -xzf "$archive" -C /build/vera; \
    bin=$(find /build/vera -name vera -type f | head -n 1); \
    test -n "$bin"; \
    install -m755 "$bin" /out/vera

FROM debian:trixie-slim

LABEL org.opencontainers.image.source="https://github.com/VeraTools/revera" \
      org.opencontainers.image.description="Self-hosted Revera pull request reviewer" \
      org.opencontainers.image.licenses="MIT"

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends git ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && groupadd --gid 10001 revera \
    && useradd --uid 10001 --gid revera --home-dir /data --no-create-home --shell /usr/sbin/nologin revera \
    && mkdir -p /data \
    && chown revera:revera /data

COPY --from=binaries /out/revera /out/vera /usr/local/bin/
COPY LICENSE /usr/share/doc/revera/LICENSE

ENV HOME=/data XDG_CACHE_HOME=/data/cache \
    REVERA_DATA_DIR=/data REVERA_LISTEN=0.0.0.0:8080
USER 10001:10001
VOLUME /data
EXPOSE 8080
ENTRYPOINT ["revera", "serve"]
