FROM debian:bookworm-slim

ARG TARGETARCH
ARG LEANCTX_VERSION=3.9.7

RUN set -eux; \
    apt-get update; \
    apt-get install -y --no-install-recommends ca-certificates curl tar findutils; \
    rm -rf /var/lib/apt/lists/*

RUN set -eux; \
    arch="${TARGETARCH:-}"; \
    if [ -z "$arch" ]; then \
      case "$(uname -m)" in \
        x86_64) arch="amd64" ;; \
        aarch64|arm64) arch="arm64" ;; \
        *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;; \
      esac; \
    fi; \
    case "$arch" in \
      amd64) target="x86_64-unknown-linux-gnu" ;; \
      arm64) target="aarch64-unknown-linux-gnu" ;; \
      *) echo "unsupported TARGETARCH=$arch" >&2; exit 1 ;; \
    esac; \
    asset="lean-ctx-${target}.tar.gz"; \
    base="https://github.com/yvgude/lean-ctx/releases/download/v${LEANCTX_VERSION}"; \
    mkdir -p /tmp/leanctx; \
    curl -fsSL -o /tmp/SHA256SUMS "${base}/SHA256SUMS"; \
    curl -fsSL -o "/tmp/${asset}" "${base}/${asset}"; \
    cd /tmp; \
    grep "  ${asset}$" SHA256SUMS | sha256sum -c -; \
    tar -xzf "/tmp/${asset}" -C /tmp/leanctx; \
    bin="$(find /tmp/leanctx -type f -name lean-ctx | head -n 1)"; \
    test -n "$bin"; \
    cp "$bin" /usr/local/bin/lean-ctx; \
    chmod 0755 /usr/local/bin/lean-ctx; \
    /usr/local/bin/lean-ctx --version; \
    rm -rf /tmp/leanctx /tmp/SHA256SUMS "/tmp/${asset}"; \
    useradd --system --uid 10001 --create-home --home-dir /var/lib/lean-ctx leanctx

USER 10001
ENV LEAN_CTX_CONFIG_DIR=/etc/lean-ctx
ENV LEAN_CTX_DATA_DIR=/var/lib/lean-ctx
EXPOSE 8484 8485
ENTRYPOINT ["lean-ctx"]
CMD ["gateway", "serve", "--port=8484", "--admin-port=8485"]
