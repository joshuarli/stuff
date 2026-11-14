# Caddy with the caddy-dns/cloudflare module, built with xcaddy.
# Both bases are pinned by multi-arch index digest (linux/amd64 is the only
# platform used); `bin/pin-verify` checks these lines against images.lock.json.
ARG BUILDER=docker.io/library/caddy:2.11.9-builder-alpine@sha256:646fee578e680989ca5c7fd1ce6f1de574d19fd71ef2221810cc0510a9f40c86
ARG RUNTIME=docker.io/library/caddy:2.11.9-alpine@sha256:003a97808e781aa9f7e75cc2f43d34d461099031faf5e20faefb9f61126836f0
ARG CADDY_VERSION=v2.11.9
ARG CLOUDFLARE_MODULE=github.com/caddy-dns/cloudflare@v0.2.4

FROM --platform=linux/amd64 ${BUILDER} AS builder
ARG CADDY_VERSION
ARG CLOUDFLARE_MODULE
RUN xcaddy build ${CADDY_VERSION} --with ${CLOUDFLARE_MODULE} --output /out/caddy \
 && /out/caddy list-modules | grep -x 'dns.providers.cloudflare' \
 && sha256sum /out/caddy

FROM --platform=linux/amd64 ${RUNTIME}
COPY --from=builder /out/caddy /usr/bin/caddy
# Bind 443 as an unprivileged user (the container is started with only
# NET_BIND_SERVICE in its bounding set).
RUN setcap cap_net_bind_service=+ep /usr/bin/caddy && getcap /usr/bin/caddy
