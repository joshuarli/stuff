# mynpm

npm archive: imports npm and pnpm lockfiles, fetches and verifies the exact
tarballs, and publishes immutable static generations (see the repository
README for serving and the exit-code contract).

## Fleet ingest

`deploy/ingest-npm-fleet.sh` imports every pnpm lockfile listed in
`deploy/npm-fleet.txt` (`<path under REPOS_DIR> <project id>`) with all six
darwin/linux targets from `mynpm init-config --all-targets` (mac-arm64,
mac-x64, linux-x64-gnu, linux-x64-musl, linux-arm64-gnu, linux-arm64-musl),
then runs `fetch` and `publish`. It never writes to R2; sync the site with
`deploy/r2-sync.sh`.

Re-running is safe. A project import is identified by its content (id,
lockfile path, targets, roots, packages, pin), so an unchanged lockfile leaves
the inventory byte-identical, while a changed one is recorded as an additional
import under the same project id and already hosted versions stay. `fetch`
reuses verified artifacts.

Packages whose `os`/`cpu`/`libc` exclude every configured target (win32,
android, freebsd, ppc64, ...) are recorded in the inventory but not fetched or
hosted.
