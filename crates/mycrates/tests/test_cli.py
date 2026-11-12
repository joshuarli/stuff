import json

from mycrates.config import Config

from .support import ScratchTestCase, cli, init_state

LEGACY_INDEX = "registry+https://github.com/rust-lang/crates.io-index"
SPARSE_INDEX = "registry+https://index.crates.io/"
CHECKSUM = "a" * 64


def lock(body, version=4):
    return f"version = {version}\n{body}"


class ImportLockTest(ScratchTestCase):
    def import_lock(self, text, *extra):
        lockfile = self.tmp / "locked" / "Cargo.lock"
        lockfile.parent.mkdir(exist_ok=True)
        lockfile.write_text(text)
        return cli("import-lock", lockfile, "--state-dir", self.tmp / "state", *extra)

    def test_only_crates_io_packages_are_pinned_and_every_other_source_is_reported(self):
        text = lock(
            f"""
[[package]]
name = "app"
version = "0.1.0"

[[package]]
name = "legacy-index"
version = "1.0.0"
source = "{LEGACY_INDEX}"
checksum = "{CHECKSUM}"

[[package]]
name = "sparse-index"
version = "2.0.0"
source = "{SPARSE_INDEX}"
checksum = "{CHECKSUM}"

[[package]]
name = "from-git"
version = "0.3.0"
source = "git+https://example.com/from-git.git#abc123"

[[package]]
name = "from-alternate"
version = "0.4.0"
source = "registry+https://alternate.example/index"
checksum = "{CHECKSUM}"

[[package]]
name = "from-path"
version = "0.5.0"
source = "path+file:///work/from-path"

[[package]]
name = "odd"
version = "0.6.0"
source = "mystery+x"
"""
        )
        code, out, err = self.import_lock(text)
        self.assertEqual(code, 0, err)
        self.assertIn("imported 2 registry packages for project 'locked'", out)
        self.assertIn("warning: 4 unsupported", out)
        self.assertIn("git: from-git 0.3.0", out)
        self.assertIn("alternate-registry (registry+https://alternate.example/index): from-alternate 0.4.0", out)
        self.assertIn("path: from-path 0.5.0", out)
        self.assertIn("unsupported: odd 0.6.0", out)
        pins = json.loads((self.tmp / "state" / "pins" / "locked.json").read_text())
        self.assertEqual([(p["name"], p["source"]) for p in pins["registry"]], [("legacy-index", LEGACY_INDEX), ("sparse-index", SPARSE_INDEX)])
        self.assertEqual(len(pins["unsupported"]), 4)
        self.assertEqual(pins["lock_content"], text)

    def test_a_bad_lockfile_fails_closed_and_writes_no_pins(self):
        pins = self.tmp / "state" / "pins" / "locked.json"
        missing = lock(f'\n[[package]]\nname = "serde"\nversion = "1.0.0"\nsource = "{LEGACY_INDEX}"\n', 3)
        code, _, err = self.import_lock(missing)
        self.assertEqual(code, 3)
        self.assertIn("serde 1.0.0 missing checksum", err)
        self.assertFalse(pins.exists())
        for text, fragment in [("version = 99\n", "unknown lock version 99"), ("version = [", ""), ("[[package]]\nname = 1\n", "")]:
            code, _, err = self.import_lock(text)
            self.assertEqual(code, 2, err)
            self.assertIn(fragment, err)
            self.assertFalse(pins.exists())
        code, _, err = cli("import-lock", self.tmp / "absent.lock", "--state-dir", self.tmp / "state")
        self.assertEqual((code, "lockfile not found" in err), (2, True))

    def test_reimporting_the_same_project_rewrites_one_pin_file(self):
        text = lock(f'\n[[package]]\nname = "a"\nversion = "1.0.0"\nsource = "{LEGACY_INDEX}"\nchecksum = "{CHECKSUM}"\n')
        self.import_lock(text)
        self.import_lock(text)
        self.assertEqual([p.name for p in (self.tmp / "state" / "pins").iterdir()], ["locked.json"])

    def test_project_names_cannot_escape_the_pins_directory(self):
        text = lock("")
        for name in ["../escape", "a/b", "."]:
            with self.subTest(name=name):
                code, _, err = self.import_lock(text, "--project", name)
                self.assertEqual(code, 2, err)
        self.assertFalse((self.tmp / "escape.json").exists())

    def test_the_default_project_is_the_lockfiles_directory_name(self):
        text = lock("")
        self.assertIn("project 'locked'", self.import_lock(text)[1])


class CommandLineTest(ScratchTestCase):
    def test_malformed_command_lines_exit_with_the_input_status(self):
        for args, fragment in [
            (["frobnicate"], "invalid choice"),
            (["--bind", "127.0.0.1:0"], ""),
            ([], "required"),
            (["publish", "--nope"], "unrecognized"),
            (["publish", "--state-dir"], "expected one argument"),
            (["config"], "required"),
            (["config", "set-public-base-url"], "required"),
            (["adopt"], "--from-site"),
        ]:
            with self.subTest(args=args):
                code, out, err = cli(*args)
                self.assertEqual(code, 2, (out, err))
                self.assertIn(fragment, err)
                self.assertEqual(out, "")

    def test_operating_system_failures_exit_with_the_operation_status_and_no_traceback(self):
        state, site = init_state(self.tmp)
        blocker = self.tmp / "blocker"
        blocker.write_text("a file where a directory must go")
        code, _, err = cli("init", "--state-dir", blocker / "state", "--site-dir", site)
        self.assertEqual(code, 1)
        self.assertTrue(err.startswith("operation: "), err)
        self.assertNotIn("Traceback", err)

    def test_abbreviated_options_are_not_accepted(self):
        self.assertEqual(cli("publish", "--state", self.tmp)[0], 2)


class ConfigCommandTest(ScratchTestCase):
    def test_init_is_idempotent_and_keeps_an_existing_config(self):
        state, site = init_state(self.tmp, "https://kept.example")
        self.assertEqual(cli("init", "--state-dir", state, "--site-dir", site)[0], 0)
        self.assertEqual(Config.load(state / "config.json").public_base_url, "https://kept.example")
        for name in ("blobs", "native", "upstream", "pins", "uploads"):
            self.assertTrue((state / name).is_dir())

    def test_config_files_use_the_documented_layout(self):
        state, _ = init_state(self.tmp)
        text = (state / "config.json").read_text()
        self.assertFalse(text.endswith("\n"))
        self.assertEqual(
            list(json.loads(text)), ["version", "public_base_url", "upstream", "limits", "auth_required"]
        )
        self.assertEqual(list(json.loads(text)["limits"])[:3], ["max_compressed", "max_expanded", "max_entries"])

    def test_the_public_base_url_must_be_https_or_loopback(self):
        state, _ = init_state(self.tmp)
        for url, code in [
            ("https://example.com/", 0), ("http://127.0.0.1:8080", 0), ("http://localhost", 0), ("http://[::1]:1", 0),
            ("http://example.com/", 2), ("https://user@example.com/", 2), ("ftp://x", 2), ("example.com", 2),
        ]:
            with self.subTest(url=url):
                self.assertEqual(cli("config", "set-public-base-url", url, "--state-dir", state)[0], code)
        self.assertEqual(Config.load(state / "config.json").public_base_url, "http://[::1]:1")

    def test_commands_refuse_a_state_directory_without_config(self):
        site = ["--site-dir", self.tmp / "site"]
        for args in (["publish", *site], ["fetch"], ["serve-publish", *site], ["config", "set-public-base-url", "https://x.test"]):
            with self.subTest(args=args):
                code, _, err = cli(*args, "--state-dir", self.tmp / "none")
                self.assertEqual(code, 2)
                self.assertIn("mycrates init", err)

    def test_config_values_are_type_checked_and_unknown_fields_rejected(self):
        state, _ = init_state(self.tmp)
        config_path = state / "config.json"
        original = json.loads(config_path.read_text())
        for mutate in [
            lambda c: c.update(version=2),
            lambda c: c.update(surprise=1),
            lambda c: c["limits"].update(max_ratio="big"),
            lambda c: c["limits"].update(redirects=True),
            lambda c: c.update(auth_required=1),
            lambda c: c.update(upstream=[]),
        ]:
            document = json.loads(json.dumps(original))
            mutate(document)
            config_path.write_text(json.dumps(document))
            with self.subTest(document=document):
                self.assertEqual(cli("publish", "--state-dir", state, "--site-dir", self.tmp / "s")[0], 2)

    def test_partial_configs_take_defaults_for_missing_fields(self):
        state, _ = init_state(self.tmp)
        (state / "config.json").write_text('{"version": 1, "public_base_url": "https://x.example"}')
        config = Config.load(state / "config.json")
        self.assertEqual(config.limits.max_ratio, 1000)
        self.assertEqual(config.upstream.index_url, "https://index.crates.io/")


class DoctorAndVerifyTest(ScratchTestCase):
    def test_doctor_reports_missing_blobs_and_unsupported_sources(self):
        state, _ = init_state(self.tmp)
        lockfile = self.tmp / "Cargo.lock"
        lockfile.write_text(
            lock(
                f'\n[[package]]\nname = "a"\nversion = "1.0.0"\nsource = "{LEGACY_INDEX}"\nchecksum = "{CHECKSUM}"\n'
                '\n[[package]]\nname = "g"\nversion = "1.0.0"\nsource = "git+https://x.test/g"\n'
            )
        )
        cli("import-lock", lockfile, "--project", "p", "--state-dir", state)
        code, out, _ = cli("doctor", "--state-dir", state)
        self.assertEqual(code, 0)
        self.assertIn("[unsupported] git: g 1.0.0", out)
        self.assertIn(f"[missing blob] a 1.0.0 ({CHECKSUM})", out)
        self.assertIn("detected 2 issues", out)

    def test_doctor_on_a_healthy_or_absent_state(self):
        state, _ = init_state(self.tmp)
        self.assertIn("system healthy", cli("doctor", "--state-dir", state)[1])
        self.assertIn("does not exist", cli("doctor", "--state-dir", self.tmp / "none")[1])

    def test_verify_exit_status_follows_the_integrity_contract(self):
        state, _ = init_state(self.tmp)
        self.assertEqual(cli("verify", "--state-dir", state)[0], 0)
        (state / "blobs" / ("f" * 64 + ".crate")).write_bytes(b"not that hash")
        code, _, err = cli("verify", "--state-dir", state)
        self.assertEqual(code, 3)
        self.assertIn("corrupted blob", err)


