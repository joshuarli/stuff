# Campaign: standardize the build repositories

Goal: every `build`-tagged repository follows [docs/build-repos.md](../build-repos.md), repomod
enforces it, and the Python build repositories share `laputa-buildlib`.

## Decisions

- Shared Python code lives in a new repository, `laputa-systems/buildlib`, published to the stuff
  PyPI registry as `laputa-buildlib`; build repositories pin it.
- Each build repository lands through one pull request, merged when its CI is green.
- Release workflows are not dispatched by the campaign; the owner triggers the first release in
  the new format.
- git-build is `build` + `shell` and follows the build release format; ssh-build's
  `git-build.lock.json` keeps pointing at the last ish-format release until git-build publishes
  a new one.
- python-build keeps its python-build-standalone assets for uv beside the standard ones.

## Slices

- [x] stuff: `build-make-targets`, `build-locks`, `build-release-standard`; `language-tags` no longer
  skips binary-release tools; vendored trees, Rust `src/target` modules, unmirrored optional npm
  packages, and compiler-rt CRT traces no longer misfire
- [x] stuff: `stuff publish pypi --wheel` hosts first-party wheels; mynpm accepts the tarball
  layouts npm installs (any root name, `./` segments, repeated files)
- [x] buildlib: laputa-buildlib 0.1.0 published to stuff PyPI, tagged v0.1.0
- [x] per repository: all eight merged (node #6, cmake #2, ffmpeg #2, chromium #5, ssh #4, pi #1,
  git #6, python #10); `repomod lint --tag build` reports only python-build's existing git sources
- [x] cmake-, node-, python-, ffmpeg-, chromium-build adopt `laputa-buildlib` (cmake #3, node #7,
  python #11, ffmpeg #3, chromium #6); plain-python3 and container contexts load the pinned wheel
  from `sources.lock.json`
- [x] `repomod topics apply`
- [x] llvm-prebuilt-musl (#3) meets the contract with the shell/jaq pattern rather than buildlib. It
  also fixes #1 and #2: each package ships compiler-rt builtins and CRT objects for both
  architectures, and the musl `-B`/`-L` paths moved from `clang.cfg` to the package triple's config

## Releases

| Repository | First shared-format release | Consumers moved |
| --- | --- | --- |
| git-build | `release-1fb0febb17073cf3fdd712c575aa1eb3a5404f91` | ssh-build, usr |
| ssh-build | `release-88b04033…` | usr; budgets re-measured (xz + 15%) |
| pi-build | `release-a17c28f2…` | usr |
| ffmpeg-build | `release-ac29c7720327f7dee4ed4677ce8421673fcc3563` | play #6, usr |
| python-build | `release-6f7c0d46e2eda7f66c705022f1950034045f74d0` (proves Alpine 3.24.2) | pi-build #2, usr #2 |
| cmake-build | qualified at `281e9cd8`, released | none pin it |
| node-build | `release-934b31ca1322638234fee6cfa8ee7ca5944f30cb` (#10 jobs per core, #11 g++ for the musl published smoke) | usr #3; budgets re-measured (xz + 15%, #12) |
| llvm-prebuilt-musl | `release-10dbf21a8bc2ec9a7f60d2f15eed2307c52c290f` (Alpine 3.24.2, clang 22 host; legacy `clang+llvm-*` assets kept beside the shared names) | chromium-build #7; bctl and hydrogen stay on their pins |
| chromium-build | none | nothing consumes the macOS release; usr's Helium installs local `helium-*` releases |

## Needs owner

- chromium-build: hosted macOS runners have 56 GiB free against the 80 GiB build and have never
  completed a smoke build. Nothing consumes the macOS release, so it waits until one does.
- llvm-prebuilt-musl: bctl (`3afbb74`) and hydrogen (`c4f9b08`) still pin older releases and have
  uncommitted work, so their bumps are left to the owner. Nothing has yet built Helium against the
  new toolchain.

## Status

Every local deliverable landed. Seven of eight repositories, plus llvm-prebuilt-musl, have
released in the shared format. chromium-build remains as listed above.
