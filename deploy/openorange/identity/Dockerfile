# Tailnet identity sidecar. Runs with no network (`--network none`): it only
# talks to tailscaled's unix socket and serves Caddy over a unix socket.
# The base is pinned by index digest; `bin/pin-verify` checks this line
# against images.lock.json.
ARG BASE=docker.io/library/python:3.14-alpine@sha256:5862d971d1f0ae26b258cefa4e893556b1d3aba81bd2ff30888704ad378bab67
FROM ${BASE}
ENV PYTHONDONTWRITEBYTECODE=1 PYTHONUNBUFFERED=1
COPY identity.py /app/identity.py
USER 10001:10001
ENTRYPOINT ["python", "-I", "/app/identity.py"]
