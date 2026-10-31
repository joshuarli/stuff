# mypypi

Python wheel index: resolves and builds wheels, validates them on the host,
and publishes immutable generations under a `current` pointer.

## Build environment

Build subprocesses keep the sanitized environment and run with a scratch
`HOME`, `CARGO_HOME`, and `RUSTUP_HOME`. The scratch homes link to the
installed Cargo bin and rustup toolchains, so Rust-backed sdists find the host
toolchain without inheriting the publisher's Cargo cache or credentials.

## Configuration

Project settings live in the configured `pyproject.toml`. In addition to the
targets under `[tool.mypypi.targets]`, `[tool.mypypi] public_base_url` names
the origin clients should use:

```toml
[tool.mypypi]
public_base_url = "https://pypi.example.net"

[tool.mypypi.targets]
platforms = ["linux-x86_64", "macos-arm64"]
linux_repair_policy = "manylinux_2_28"
```

The publisher writes that origin into the generation's `llms.txt`, so a
deployment at another origin publishes pip instructions for its own index.
When the setting is absent it defaults to `https://pypi.17166969.xyz`.

## Publication format

Generations are identified by `mypypi-simple-html-v5`. Older
`mypypi-simple-html-v4` generations stay readable and are not rewritten.
