# Production catalogs

The four `catalog.json` files the live registries served on 2026-11-09
(mycrates generation `9ed854790d77710a3a17cdaf`: 1217 crates; mynpm: 329
packages; mypypi: 39 projects; oci: 2 repositories). They are the
production-scale input for `crates/stuff-worker/benches/request_paths.rs`.
To refresh them, read `current` and then `generations/<id>/catalog.json`
from each bucket with `cf r2 objects get --bucket-name stuff-<registry> KEY --text`.
