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

## Needs owner

- First release dispatch per repository; nothing has been released in the new format.
- After those releases, bump the consumers that pin old assets: pi-build's python pins in
  `bootstrap.lock.json`/`sources.lock.json` (python-build), play's Linux Dockerfile and
  `ffmpeg-sdk.lock.json` (ffmpeg-build), ssh-build's `git-build.lock.json` and
  `scripts/fetch-git-build.sh` (git-build: `git-<version>-<triple>.tar.xz`, `tar -xJf`).
- `usr/tools/sync/pi_release.py` and the `dl` installers read the old pi-build, node-build, and
  ssh-build tags and asset names.
- Old release evidence (cmake qualification and memory baselines) no longer matches; the next
  qualify run re-measures.
- Re-measure `release-budgets.json` ceilings on the first xz builds (node, ssh, cmake).
- python-build's and node-build's Linux builders moved from Alpine edge to the approved 3.24.2
  image; only a full Linux build proves them.

## Status

Closed: every local deliverable landed. Open items are the owner list above.
