import hashlib
from dataclasses import dataclass
from pathlib import Path

from . import catalog as catalog_module
from .errors import InputError, IntegrityError
from .fsutil import sync_dir, write_file_durable
from .index import mirror_config_json, native_config_json
from .model import IndexEntry
from .names import lower_name, shard_for_name, validate_crate_name
from .semver import compare_key
from .site import Site, sync_immutable_file
from .wirejson import loads

RENDERER_VERSION = 4
GENERATION_ID_LENGTH = 24


@dataclass(frozen=True)
class PublishResult:
    generation_id: str
    is_noop: bool


def _sha256(data):
    return hashlib.sha256(data).hexdigest()


def _catalog_dependencies(entry):
    dependencies = catalog_module.Dependencies()
    for dep in entry.deps:
        name = dep.package or dep.name
        if dep.kind in (None, "normal"):
            dependencies.runtime.append(name)
        elif dep.kind == "build":
            dependencies.build.append(name)
        elif dep.kind == "dev":
            dependencies.dev.append(name)
    return dependencies


def _catalog_inputs(native_records, upstream_records):
    inputs = []
    for record in native_records:
        entry = record.entry
        inputs.append(
            catalog_module.CargoInput(
                entry.name, entry.vers, entry.yanked, "native", _catalog_dependencies(entry)
            )
        )
    for record in upstream_records:
        try:
            entry = IndexEntry.from_obj(loads(record.raw_index_line))
        except InputError:
            entry = None
        inputs.append(
            catalog_module.CargoInput(
                record.name,
                record.vers,
                bool(entry and entry.yanked),
                "upstream",
                _catalog_dependencies(entry) if entry else catalog_module.Dependencies(),
            )
        )
    return inputs


class Publisher:
    def __init__(self, store, site_dir, public_base_url, auth_required=False):
        self.store = store
        self.site_dir = Path(site_dir)
        self.public_base_url = public_base_url
        self.auth_required = auth_required

    @property
    def blobs_dir(self):
        return self.site_dir / "blobs"

    def _sync_blob(self, checksum):
        sync_immutable_file(
            self.store.blob_path(checksum), self.blobs_dir / f"{checksum}.crate", checksum
        )

    def _generation_matches(self, generation_dir, manifest):
        for (view, relative), expected in manifest.items():
            path = generation_dir / view / relative if view else generation_dir / relative
            try:
                data = path.read_bytes()
            except OSError:
                return False
            if _sha256(data) != expected:
                return False
        return True

    def publish(self):
        site = Site(self.site_dir)
        site.recover_staging()
        site.check_no_symlinks_in_path()
        site.ensure_layout()
        self.blobs_dir.mkdir(parents=True, exist_ok=True)
        sync_dir(self.blobs_dir)

        native_records = self.store.load_native_records()
        upstream_records = self.store.load_upstream_records()
        for record in native_records:
            self._sync_blob(record.entry.cksum)
        for record in upstream_records:
            self._sync_blob(record.checksum)

        files = {}
        self._render_index(native_records, upstream_records, files)
        catalog = catalog_module.cargo_catalog(
            catalog_module.FINGERPRINT_GENERATION, _catalog_inputs(native_records, upstream_records)
        )
        fingerprint = catalog_module.catalog_fingerprint(catalog, self.public_base_url)
        manifest = {key: _sha256(data) for key, data in files.items()}
        canonical = f"v:{RENDERER_VERSION}\ncatalog:{fingerprint}\n" + "".join(
            f"{view}|{path}|{sha}\n" for (view, path), sha in sorted(manifest.items())
        )
        generation_id = _sha256(canonical.encode("utf-8"))[:GENERATION_ID_LENGTH]

        catalog.generation = generation_id
        for name, text in (
            ("catalog.json", catalog_module.render_catalog_json(catalog)),
            ("index.html", catalog_module.render_html(catalog)),
            ("llms.txt", catalog_module.render_llms_txt(catalog, self.public_base_url)),
        ):
            data = text.encode("utf-8")
            files[("", name)] = data
            manifest[("", name)] = _sha256(data)

        def build(staging):
            for (view, relative), data in files.items():
                target = staging / view / relative if view else staging / relative
                write_file_durable(target, data)

        activation = site.publish(
            generation_id, build, lambda generation_dir: self._generation_matches(generation_dir, manifest)
        )
        return PublishResult(activation.generation_id, activation.is_noop)

    def _render_index(self, native_records, upstream_records, files):
        files[("registry", "config.json")] = native_config_json(self.public_base_url, self.auth_required).encode()
        files[("crates-io", "config.json")] = mirror_config_json(self.public_base_url).encode()

        native_by_crate = {}
        for record in native_records:
            validate_crate_name(record.entry.name)
            native_by_crate.setdefault(lower_name(record.entry.name), []).append(record)
        for crate, records in sorted(native_by_crate.items()):
            records.sort(key=lambda r: compare_key(r.entry.vers))
            lines = [record.entry.index_line() for record in records]
            files[("registry", _shard(crate))] = ("\n".join(lines) + "\n").encode("utf-8")

        upstream_by_crate = {}
        for record in upstream_records:
            validate_crate_name(record.name)
            upstream_by_crate.setdefault(lower_name(record.name), []).append(record)
        for crate, records in sorted(upstream_by_crate.items()):
            records.sort(key=lambda r: compare_key(r.vers))
            lines = [record.raw_index_line.strip() for record in records]
            files[("crates-io", _shard(crate))] = ("\n".join(lines) + "\n").encode("utf-8")


def _shard(crate_lower):
    shard = shard_for_name(crate_lower)
    if shard is None:
        raise IntegrityError(f"crate name is not ASCII: {crate_lower}")
    return shard

