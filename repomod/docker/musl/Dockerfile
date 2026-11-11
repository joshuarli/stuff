ARG BASE_IMAGE=alpine:3.24.2@sha256:294b683cb724975bec92580e1e685676bd4b50bda910ddb8c51d4cabeaec77e6
FROM ${BASE_IMAGE}

ARG TARGETARCH
ARG MUSL_VERSION=1.2.6
ARG MUSL_SHA256=d585fd3b613c66151fc3249e8ed44f77020cb5e6c1e635a616d3f9f82460512a
ARG LLVM_VERSION=23.1.5
ARG LLVM_RELEASE_SHA=4c2254e
ARG LLVM_X86_64_SHA256=9f599abd0caf81df64fdf70442ef2c39cb701d346030643193b39a743ad43ce0
ARG LLVM_AARCH64_SHA256=187e71574399adaedd6077ffb5e4b20900aa29f32c3f131db2f4440273b75444
ARG MOLD_VERSION=3.0.0
ARG MOLD_RELEASE_SHA=cfb4aab
ARG MOLD_X86_64_SHA256=d6e47d61b74961a60868ad308acad4dfdc199e7ec97d61d65728e6ef1d6c2565
ARG MOLD_AARCH64_SHA256=2d904b170d804008ada80157f8f18d9920d3ccc1e49e27d80123d01e386668f3
ARG RUSTUP_VERSION=1.29.1
ARG RUSTUP_X86_64_SHA256=331228566cc931f32cd684f9bebc5956a5e5d7394c8d5de595f196a658ab98e6
ARG RUSTUP_AARCH64_SHA256=1ddf36182ac5d1782dbeefcb9bea7f5f9412d88f4a0d5047b1564a223624e8c6

RUN apk add --no-cache \
    bash=5.3.9-r1 \
    binutils=2.45.1-r1 \
    ca-certificates=20261020-r0 \
    coreutils=9.11-r0 \
    curl=8.23.0-r0 \
    file=5.47-r2 \
    git=2.54.1-r0 \
    libgcc=15.2.0-r5 \
    make=4.4.1-r4 \
    musl-dev=1.2.6-r3 \
    openssl=3.5.9-r0 \
    pax-utils=1.3.9-r1 \
    python3=3.14.10-r1 \
    tar=1.35-r5 \
    tzdata=2026f-r0 \
    xz=5.8.4-r0

RUN ar rcs /usr/lib/libutil.a

RUN case "$TARGETARCH" in \
        amd64) arch=x86_64; sum="$LLVM_X86_64_SHA256" ;; \
        arm64) arch=aarch64; sum="$LLVM_AARCH64_SHA256" ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl -fsSL -o /tmp/llvm.tar.xz "https://github.com/laputa-systems/llvm-prebuilt-musl/releases/download/llvm-musl-${LLVM_VERSION}-${LLVM_RELEASE_SHA}/clang+llvm-${LLVM_VERSION}-${arch}-linux-musl.tar.xz" \
    && echo "$sum  /tmp/llvm.tar.xz" | sha256sum -c - \
    && mkdir -p /opt/llvm-musl \
    && tar -xJf /tmp/llvm.tar.xz -C /opt/llvm-musl --strip-components=1 \
    && test -f /opt/llvm-musl/lib/libclang.so \
    && rm /tmp/llvm.tar.xz

RUN for target in x86_64-unknown-linux-musl aarch64-unknown-linux-musl; do \
        stub_dir="/usr/lib/musl-crt/$target"; \
        mkdir -p "$stub_dir"; \
        for obj in crtbegin.o crtbeginS.o crtbeginT.o crtend.o crtendS.o; do \
            /opt/llvm-musl/bin/clang --target="$target" -x c -c /dev/null -o "$stub_dir/$obj"; \
        done; \
    done

RUN case "$TARGETARCH" in \
        amd64) arch=x86_64 ;; \
        arm64) arch=aarch64 ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl -fsSL -o /tmp/musl.tar.gz "https://musl.libc.org/releases/musl-${MUSL_VERSION}.tar.gz" \
    && echo "$MUSL_SHA256  /tmp/musl.tar.gz" | sha256sum -c - \
    && mkdir /tmp/musl-src \
    && tar -xzf /tmp/musl.tar.gz -C /tmp/musl-src --strip-components=1 \
    && cd /tmp/musl-src \
    && CC=/opt/llvm-musl/bin/clang \
        AR=/opt/llvm-musl/bin/llvm-ar \
        RANLIB=/opt/llvm-musl/bin/llvm-ranlib \
        LIBCC="$(/opt/llvm-musl/bin/clang --rtlib=compiler-rt --print-libgcc-file-name)" \
        ./configure --prefix=/opt/musl-llvm --target="$arch" --disable-shared \
    && make -j"$(nproc)" \
    && make install \
    && stub_dir="/usr/lib/musl-crt/${arch}-unknown-linux-musl" \
    && for obj in libc.a crt1.o rcrt1.o Scrt1.o crti.o crtn.o; do \
        cp "/opt/musl-llvm/lib/$obj" "/usr/lib/$obj"; \
    done \
    && cp /opt/llvm-musl/lib/libunwind.a "$stub_dir/libunwind.a" \
    && for config in clang.cfg clang++.cfg; do \
        printf '%s\n' "-B$stub_dir" "-L$stub_dir" --rtlib=compiler-rt --unwindlib=none >> "/opt/llvm-musl/bin/$config"; \
    done \
    && cd / \
    && rm -rf /tmp/musl.tar.gz /tmp/musl-src /opt/musl-llvm

RUN case "$TARGETARCH" in \
        amd64) arch=x86_64; sum="$MOLD_X86_64_SHA256" ;; \
        arm64) arch=aarch64; sum="$MOLD_AARCH64_SHA256" ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl -fsSL -o /tmp/mold.tar.gz "https://github.com/joshuarli/mold/releases/download/mold-musl-${MOLD_VERSION}-${MOLD_RELEASE_SHA}/mold-${MOLD_VERSION}-${arch}-linux-musl.tar.gz" \
    && echo "$sum  /tmp/mold.tar.gz" | sha256sum -c - \
    && mkdir -p /opt/mold \
    && tar -xzf /tmp/mold.tar.gz -C /opt/mold --strip-components=1 \
    && /opt/mold/bin/mold --version \
    && rm /tmp/mold.tar.gz

RUN case "$TARGETARCH" in \
        amd64) arch=x86_64; sum="$RUSTUP_X86_64_SHA256" ;; \
        arm64) arch=aarch64; sum="$RUSTUP_AARCH64_SHA256" ;; \
        *) echo "unsupported TARGETARCH: $TARGETARCH" >&2; exit 1 ;; \
    esac \
    && curl -fsSL -o /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/${arch}-unknown-linux-musl/rustup-init" \
    && echo "$sum  /tmp/rustup-init" | sha256sum -c - \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --default-toolchain none \
    && rm /tmp/rustup-init

ENV PATH="/opt/llvm-musl/bin:/opt/mold/bin:/root/.cargo/bin:$PATH" \
    CC="/opt/llvm-musl/bin/clang" \
    AR="/opt/llvm-musl/bin/llvm-ar" \
    RANLIB="/opt/llvm-musl/bin/llvm-ranlib" \
    LIBRARY_PATH="/opt/llvm-musl/lib" \
    LIBCLANG_PATH="/opt/llvm-musl/lib" \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER="/opt/llvm-musl/bin/clang" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER="/opt/llvm-musl/bin/clang"

COPY rust-toolchain.toml /tmp/rust-toolchain.toml
RUN channel="$(awk -F'"' '/^channel/ {print $2}' /tmp/rust-toolchain.toml)" \
    && test -n "$channel" \
    && rustup toolchain install "$channel" \
        --target x86_64-unknown-linux-musl \
        --target aarch64-unknown-linux-musl \
        --component rust-src \
    && rustup default "$channel" \
    && rm /tmp/rust-toolchain.toml

RUN host_target="$(rustc -vV | awk '/^host:/ {print $2}')" \
    && host_libdir="$(rustc --print target-libdir)" \
    && for obj in crtbegin.o crtbeginS.o crtbeginT.o crtend.o crtendS.o; do \
        ln -sf "/usr/lib/musl-crt/$host_target/$obj" "$host_libdir/$obj"; \
    done \
    && for obj in crt1.o rcrt1.o Scrt1.o crti.o crtn.o; do \
        ln -sf "/usr/lib/$obj" "$host_libdir/self-contained/$obj"; \
    done

RUN host_libdir="$(rustc --print target-libdir)" \
    && ln -sf /usr/lib/libgcc_s.so.1 "$host_libdir/libgcc_s.so" \
    && ln -sf /usr/lib/libgcc_s.so.1 "$host_libdir/libgcc_s.so.1" \
    && ln -sf /usr/lib/libc.so "$host_libdir/libc.so"

RUN cat > /usr/local/bin/musl-cargo <<'EOF' \
    && chmod +x /usr/local/bin/musl-cargo
#!/bin/sh
set -eu

target=${MUSL_TARGET:?MUSL_TARGET is required}
case "$target" in
    x86_64-unknown-linux-musl)
        crt=/usr/lib/musl-crt/x86_64-unknown-linux-musl
        ;;
    aarch64-unknown-linux-musl)
        crt=/usr/lib/musl-crt/aarch64-unknown-linux-musl
        ;;
    *)
        exec cargo "$@"
        ;;
esac

mode=${MUSL_BUILD_MODE:-static}
native="-L native=$crt -L native=/usr/lib -Clink-arg=-B$crt -Clink-arg=--ld-path=/opt/mold/bin/mold"
release="-Cpanic=abort"
case "$mode" in
    static|test|driver)
        flags=$native
        ;;
    release-static)
        flags="$native $release -Ctarget-feature=+crt-static"
        ;;
    *)
        echo "musl-cargo: unknown build mode: $mode" >&2
        exit 2
        ;;
esac

target_env=$(printf '%s' "$target" | tr '[:lower:]-' '[:upper:]_')
linker_var="CARGO_TARGET_${target_env}_LINKER"
rustflags_var="CARGO_TARGET_${target_env}_RUSTFLAGS"
export "$linker_var=clang"
if [ "$mode" = driver ] || [ "$mode" = test ]; then
    unset CARGO_ENCODED_RUSTFLAGS
    export RUSTFLAGS="$flags"
else
    export "$rustflags_var=$flags"
fi
exec cargo "$@"
EOF

COPY verify-static-elf /usr/local/bin/verify-static-elf
RUN chmod +x /usr/local/bin/verify-static-elf

WORKDIR /work
