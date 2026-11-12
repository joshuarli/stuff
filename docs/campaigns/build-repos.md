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

- [ ] stuff: `build-release-standard`, `build-make-targets`, `build-locks` rules; `language-tags`
  no longer skips binary-release tools; git-build tags
- [ ] buildlib: repository, lock readers, release manifest, checksums, Mach-O and ELF checks;
  published to stuff PyPI
- [ ] per repository (chromium, cmake, ffmpeg, git, node, pi, python, ssh): make targets, lock
  schema, `release.yml` in the standard format, lint workflow, open lint findings
- [ ] cmake-, node-, python-, ffmpeg-, chromium-build adopt `laputa-buildlib`
- [ ] `repomod topics apply`

## Needs owner

- First release dispatch per repository, then bump the consumers that pin old assets: pi-build's
  `tools/toolchain.pins` (python-build), play's Linux Dockerfile (ffmpeg-build), ssh-build's
  `git-build.lock.json` (git-build), chromium-build's `helium.inputs.lock` (its own Rust toolchain
  release).

## Status

Spec written; starting the stuff rules, buildlib, and the per-repository migrations.
