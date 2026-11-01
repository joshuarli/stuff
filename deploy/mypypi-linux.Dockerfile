FROM debian:trixie-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl build-essential pkg-config git procps socat && rm -rf /var/lib/apt/lists/*
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/usr/local/bin:$PATH
COPY uv-version /tmp/uv-version
RUN UV_VERSION=$(cat /tmp/uv-version) && curl -fsSL "https://astral.sh/uv/$UV_VERSION/install.sh" | env UV_INSTALL_DIR=/usr/local/bin UV_NO_MODIFY_PATH=1 sh
RUN UV_PYTHON_INSTALL_DIR=/opt/pbs uv python install 3.15 && cp -a /opt/pbs/cpython-3.15*/. /usr/local/ && rm -rf /opt/pbs && python3 --version
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
RUN rustup toolchain install nightly-2026-09-15 --profile minimal && rustup default nightly-2026-09-15
