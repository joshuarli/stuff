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

## Releases

| Repository | First shared-format release | Consumers moved |
| --- | --- | --- |
| git-build | `release-1fb0febb17073cf3fdd712c575aa1eb3a5404f91` | ssh-build, usr |
| ssh-build | `release-88b04033…` | usr; budgets re-measured (xz + 15%) |
| pi-build | `release-a17c28f2…` | usr |
| ffmpeg-build | `release-ac29c7720327f7dee4ed4677ce8421673fcc3563` | play #6, usr |
| python-build | `release-6f7c0d46e2eda7f66c705022f1950034045f74d0` (proves Alpine 3.24.2) | pi-build #2, usr #2 |
| cmake-build | qualified at `281e9cd8`, released | none pin it |
| node-build | building (#9 fixed `TARGET` leaking into gyp sub-makes; #10 sets jobs per core) | usr #3 waits for it |
| chromium-build | none | Helium in usr waits for it |

## Needs owner

- chromium-build: hosted macOS runners have 56 GiB free against the 80 GiB build and have never
  completed a smoke build; a release needs a local or self-hosted build.
- node-build: after the first release, merge usr #3 and re-measure `release-budgets.json` on the
  xz sizes.

## Status

Every local deliverable landed and six of eight repositories released in the shared format; node-build
and chromium-build remain as listed above.
