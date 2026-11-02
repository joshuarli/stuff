ARG MANYLINUX_IMAGE
FROM ${MANYLINUX_IMAGE}

ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:/usr/local/bin:$PATH
COPY uv-version mypypi-linux-python-version mypypi-repair-versions provision-mypypi-repair.sh /tmp/
RUN UV_VERSION=$(cat /tmp/uv-version) && curl -fsSL "https://astral.sh/uv/$UV_VERSION/install.sh" | env UV_INSTALL_DIR=/usr/local/bin UV_NO_MODIFY_PATH=1 sh
RUN set -eu; PYTHON_VERSION=$(cat /tmp/mypypi-linux-python-version); UV_PYTHON_INSTALL_DIR=/opt/pbs uv python install "$PYTHON_VERSION"; cp -a /opt/pbs/cpython-"$PYTHON_VERSION"-*/. /usr/local/; rm -rf /opt/pbs; python3 --version
RUN PYTHON=/usr/local/bin/python3 sh /tmp/provision-mypypi-repair.sh
RUN curl -fsSL https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain none
RUN rustup toolchain install nightly-2026-09-15 --profile minimal && rustup default nightly-2026-09-15
