FROM alpine:3.24.2@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6

ARG TARGETARCH
ARG LLVM_VERSION=23.1.0-rc2
ARG LLVM_RELEASE_SHA=6eb5fb9
ARG MOLD_VERSION=3.0.0
ARG MOLD_RELEASE_SHA=cfb4aab
ARG RUSTUP_VERSION=1.29.1

RUN apk add --no-cache \
    bash=5.3.9-r1 \
    make=4.4.1-r4 \
    mold=2.39.1-r0 \
    musl-dev=1.2.6-r3

RUN wget -O /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-musl/rustup-init" \
    && echo eeeeeeee | sha256sum -c -

RUN wget -O /tmp/llvm.tar.xz "https://github.com/laputa-systems/llvm-prebuilt-musl/releases/download/llvm-musl-${LLVM_VERSION}-${LLVM_RELEASE_SHA}/clang+llvm-${LLVM_VERSION}-x86_64-linux-musl.tar.xz" \
    && echo aaaaaaaa | sha256sum -c -

RUN wget -O /tmp/mold.tar.gz "https://github.com/joshuarli/mold/releases/download/mold-musl-${MOLD_VERSION}-${MOLD_RELEASE_SHA}/mold-${MOLD_VERSION}-x86_64-linux-musl.tar.gz" \
    && echo cccccccc | sha256sum -c -

RUN rustup toolchain install nightly \
    --target x86_64-unknown-linux-musl \
    --target aarch64-unknown-linux-musl \
    --component rust-src

RUN cat > /usr/local/bin/musl-cargo <<'EOF'
#!/bin/sh
exec cargo "$@"
EOF

RUN for obj in crtbegin.o crtbeginS.o crtend.o crtendS.o; do echo "$obj"; done
RUN ar rcs /usr/lib/libutil.a
