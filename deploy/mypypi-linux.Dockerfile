ARG MANYLINUX_IMAGE
FROM ${MANYLINUX_IMAGE}

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/usr/local/bin:$PATH
COPY uv-version mypypi-linux-python-version mypypi-repair-versions provision-mypypi-repair.sh /tmp/
RUN UV_VERSION=$(cat /tmp/uv-version) && curl -fsSL "https://astral.sh/uv/$UV_VERSION/install.sh" | env UV_INSTALL_DIR=/usr/local/bin UV_NO_MODIFY_PATH=1 sh
RUN set -eu; PYTHON_VERSION=$(cat /tmp/mypypi-linux-python-version); UV_PYTHON_INSTALL_DIR=/opt/pbs uv python install "$PYTHON_VERSION"; cp -a /opt/pbs/cpython-"$PYTHON_VERSION"-*/. /usr/local/; rm -rf /opt/pbs; python3 --version
RUN PYTHON=/usr/local/bin/python3 sh /tmp/provision-mypypi-repair.sh
ARG RUSTUP_VERSION=1.29.3
RUN set -eu; arch=$(uname -m); \
    case "$arch" in \
      x86_64) sum=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55 ;; \
      aarch64) sum=9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282 ;; \
      *) echo "unsupported architecture $arch" >&2; exit 1 ;; \
    esac; \
    curl --proto '=https' --tlsv1.2 -fsSL -o /tmp/rustup-init "https://static.rust-lang.org/rustup/archive/$RUSTUP_VERSION/$arch-unknown-linux-gnu/rustup-init"; \
    echo "$sum  /tmp/rustup-init" | sha256sum -c -; \
    chmod +x /tmp/rustup-init; \
    /tmp/rustup-init -y --profile minimal --default-toolchain none; \
    rm /tmp/rustup-init
RUN rustup toolchain install nightly-2026-09-15 --profile minimal && rustup default nightly-2026-09-15
