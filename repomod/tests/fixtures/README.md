# Captured policy fixtures

Workflow and manifest samples were read from these default-branch commits on 2026-10-22.
Each fixture contains only the files needed by its offline policy test.

| Repository | Commit | Captured files |
| --- | --- | --- |
| `laputa-systems/srv` | `2d5f4216dbb01a6305c759777c2a6342ffb2e39f` | release workflow, Cargo manifest, Makefile |
| `joshuarli/ish` | `dfb615457ad4828216d9d8b8ea44711e8806b633` | release workflow, Cargo manifest, Makefile |
| `laputa-systems/diff-pretty` | `6da54f5a971416c345531bb597ec980479b11d69` | release workflow, Cargo manifest, Makefile |
| `laputa-systems/xsh` | `c1237e3a60fd8fd1d69027b96d22f148d1052f89` | release workflow, Cargo manifest |
| `joshuarli/mactop` | `957bec65023d99c58bf71f5fee27dbc7fd3807df` | release workflow, Swift package manifest |
| `joshuarli/gh` | `861d9894f6257b4c378bd7cd41b2dc232ce82bf1` | release workflow, Go module, command entry point |
| `laputa-systems/zzz` | `bf84f123686f560edbf67624c3c99080388472f5` | release workflow, Makefile, Zig entry point |
| `joshuarli/pytest-parallel` | `8ddd4ce26556f061092a147af5b250747b7499d0` | release workflow, Python project metadata |
| `joshuarli/python-build` | `4752d8eda0cd3166bd042d994216b4f2a03137e9` | nested Rust manifests and Python build-module shape |

The `src/main.rs` files in the `srv`, `ish`, and `diff-pretty` fixtures are tiny test stubs;
the production workflow, manifest, and Makefile samples remain captured. The Python build
modules are a concise typed fixture based on the nested structure at the listed commit; the
Cargo manifests and Rust module are captured. The fixtures are inputs to local policy checks
and are never executed.
