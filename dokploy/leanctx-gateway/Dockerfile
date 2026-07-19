FROM debian:bookworm-slim

ARG TARGETARCH
ARG LEANCTX_VERSION

COPY install-leanctx.sh /usr/local/bin/install-leanctx
COPY setup-image.sh /usr/local/bin/setup-image
COPY team-sync.sh /usr/local/bin/team-sync

RUN setup-image "$LEANCTX_VERSION" "$TARGETARCH" \
 && chmod +x /usr/local/bin/team-sync \
 && mkdir -p /srv/repos \
 && chown 10001:10001 /srv/repos

USER 10001
ENV LEAN_CTX_CONFIG_DIR=/etc/lean-ctx
ENV LEAN_CTX_GATEWAY_KEYS=/etc/lean-ctx/gateway-keys.toml
ENV LEAN_CTX_DATA_DIR=/var/lib/lean-ctx
EXPOSE 8484 8485 8686
ENTRYPOINT ["lean-ctx"]
CMD ["gateway", "serve", "--port=8484", "--admin-port=8485"]
