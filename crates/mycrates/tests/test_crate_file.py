import io
import tarfile

from mycrates.crate_file import ScanLimits, parse_packaged_manifest, scan_crate_staged
from mycrates.errors import IntegrityError, LimitError

from .support import AssertMixin, build_archive, gzip_bytes, make_crate, sha256

LIMITS = ScanLimits()


def scan(data, name="demo", version="0.1.0", limits=LIMITS, expected=None):
    return scan_crate_staged(
        io.BytesIO(data), len(data), limits.max_compressed, expected or sha256(data), name, version, limits
    )


def manifest(name="demo", version="0.1.0"):
    return f'[package]\nname = "{name}"\nversion = "{version}"\n'.encode()


def tar_with(entries):
    raw = io.BytesIO()
    with tarfile.open(fileobj=raw, mode="w", format=tarfile.GNU_FORMAT) as archive:
        for info, data in entries:
            archive.addfile(info, io.BytesIO(data) if data is not None else None)
    return gzip_bytes(raw.getvalue())


def file_info(path, size):
    info = tarfile.TarInfo(path)
    info.size = size
    return info


class ScanTest(AssertMixin):
    def test_a_well_formed_crate_yields_its_checksum_and_manifest(self):
        data = make_crate("demo", "0.1.0")
        result = scan(data)
        self.assertEqual(result.checksum, sha256(data))
        self.assertIn('name = "demo"', result.cargo_toml)

    def test_directory_entries_under_the_prefix_are_accepted(self):
        directory = tarfile.TarInfo("demo-0.1.0/")
        directory.type = tarfile.DIRTYPE
        data = tar_with([(directory, None), (file_info("demo-0.1.0/Cargo.toml", len(manifest())), manifest())])
        self.assertIn("demo", scan(data).cargo_toml)

    def test_entries_outside_the_name_version_prefix_are_rejected(self):
        for path in ["other-0.1.0/Cargo.toml", "demo-0.1.0x/Cargo.toml", "Cargo.toml"]:
            data = tar_with([(file_info(path, len(manifest())), manifest())])
            with self.subTest(path=path), self.assertRaises(IntegrityError):
                scan(data)

    def test_path_traversal_and_absolute_paths_are_rejected(self):
        for path in ["demo-0.1.0/../evil", "/etc/passwd", "demo-0.1.0/a/../../b", "demo-0.1.0//x", "demo-0.1.0/./x"]:
            data = tar_with(
                [(file_info("demo-0.1.0/Cargo.toml", len(manifest())), manifest()), (file_info(path, 1), b"x")]
            )
            with self.subTest(path=path), self.assertRaises(IntegrityError):
                scan(data)

    def test_duplicate_paths_are_rejected(self):
        data = tar_with(
            [
                (file_info("demo-0.1.0/Cargo.toml", len(manifest())), manifest()),
                (file_info("demo-0.1.0/src/lib.rs", 1), b"a"),
                (file_info("demo-0.1.0/src/lib.rs", 1), b"b"),
            ]
        )
        with self.assertRaisesRegex(IntegrityError, "duplicate archive path"):
            scan(data)

    def test_only_regular_files_and_directories_are_allowed(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE):
            link = tarfile.TarInfo("demo-0.1.0/link")
            link.type = kind
            link.linkname = "/etc/passwd"
            data = tar_with([(file_info("demo-0.1.0/Cargo.toml", len(manifest())), manifest()), (link, None)])
            with self.subTest(kind=kind), self.assertRaisesRegex(IntegrityError, "unsupported entry type"):
                scan(data)

    def test_the_manifest_must_exist_once_and_agree_with_the_requested_identity(self):
        no_manifest = tar_with([(file_info("demo-0.1.0/src/lib.rs", 1), b"x")])
        with self.assertRaisesRegex(IntegrityError, "missing Cargo.toml"):
            scan(no_manifest)
        duplicate = tar_with([(file_info("demo-0.1.0/Cargo.toml", len(manifest())), manifest())] * 2)
        with self.assertRaises(IntegrityError):
            scan(duplicate)
        wrong = tar_with([(file_info("demo-0.1.0/Cargo.toml", len(manifest("evil"))), manifest("evil"))])
        with self.assertRaisesRegex(IntegrityError, "name/version mismatch"):
            scan(wrong)
        broken = tar_with([(file_info("demo-0.1.0/Cargo.toml", 5), b"[[[[[")])
        with self.assertRaises(IntegrityError):
            scan(broken)

    def test_limits_are_enforced_as_limit_errors(self):
        data = make_crate("demo", "0.1.0", files={"big": b"\0" * 200_000})
        with self.assertRaisesRegex(LimitError, "expansion ratio"):
            scan(data, limits=ScanLimits(max_ratio=10))
        with self.assertRaisesRegex(LimitError, "expanded size"):
            scan(data, limits=ScanLimits(max_expanded=1000))
        with self.assertRaisesRegex(LimitError, "too many entries"):
            scan(data, limits=ScanLimits(max_entries=2))
        with self.assertRaisesRegex(LimitError, "compressed size"):
            scan(data, limits=ScanLimits(max_compressed=100))
        oversized = tar_with([(file_info("demo-0.1.0/Cargo.toml", 600), manifest() + b"#" * 560)])
        with self.assertRaisesRegex(LimitError, "manifest too large"):
            scan(oversized, limits=ScanLimits(max_manifest_bytes=100))

    def test_checksum_and_length_are_verified_against_the_caller(self):
        data = make_crate("demo", "0.1.0")
        with self.assertRaisesRegex(IntegrityError, "checksum mismatch"):
            scan(data, expected="0" * 64)
        with self.assertRaisesRegex(IntegrityError, "length mismatch"):
            scan_crate_staged(io.BytesIO(data), len(data) + 1, 10**9, None, "demo", "0.1.0", LIMITS)

    def test_damaged_archives_are_integrity_errors(self):
        data = make_crate("demo", "0.1.0")
        for damaged in [data[: len(data) // 2], b"not gzip at all", gzip_bytes(b"not a tar" * 100), data[:40] + bytes(b ^ 0xFF for b in data[40:80]) + data[80:]]:
            with self.subTest(size=len(damaged)), self.assertRaises(IntegrityError):
                scan(damaged)


class PackagedManifestTest(AssertMixin):
    def test_sections_targets_kinds_and_renames_are_collected(self):
        parsed = parse_packaged_manifest(
            '[package]\nname = "p"\nversion = "1.0.0"\nlinks = "l"\nrust-version = "1.70"\n'
            '[dependencies]\na = "1"\nb = { package = "real", version = "^2", features = ["x"], optional = true, default-features = false }\n'
            '[dev-dependencies]\nc = "3"\n[build-dependencies]\nd = "4"\n'
            "[target.'cfg(unix)'.dependencies]\ne = \"5\"\n[features]\ndefault = [\"x\"]\n"
        )
        self.assertEqual((parsed.links, parsed.rust_version), ("l", "1.70"))
        self.assertEqual(parsed.features, {"default": ["x"]})
        found = {(d.alias, d.package, d.kind, d.target, d.optional, d.default_features) for d in parsed.dependencies}
        self.assertEqual(
            found,
            {
                ("a", None, None, None, False, True),
                ("b", "real", None, None, True, False),
                ("c", None, "dev", None, False, True),
                ("d", None, "build", None, False, True),
                ("e", None, None, "cfg(unix)", False, True),
            },
        )

    def test_git_and_path_dependencies_cannot_be_published(self):
        for spec in ['{ git = "https://example.test/x" }', '{ path = "../x", version = "1" }']:
            text = f'[package]\nname = "p"\nversion = "1.0.0"\n[dependencies]\nx = {spec}\n'
            with self.subTest(spec=spec), self.assertRaisesRegex(IntegrityError, "git/path"):
                parse_packaged_manifest(text)

    def test_unknown_kinds_target_sections_and_bad_requirements_are_rejected(self):
        base = '[package]\nname = "p"\nversion = "1.0.0"\n'
        for body in [
            '[dependencies]\nx = { version = "1", kind = "weird" }\n',
            '[target.\'cfg(unix)\'.patch]\nx = "1"\n',
            '[dependencies]\nx = "not a version"\n',
            "[dependencies]\nx = 5\n",
            '[features]\nf = "not a list"\n',
        ]:
            with self.subTest(body=body), self.assertRaises(IntegrityError):
                parse_packaged_manifest(base + body)


