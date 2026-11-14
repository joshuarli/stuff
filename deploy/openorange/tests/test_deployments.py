import os
import runpy
from dataclasses import replace
from pathlib import Path
import sys
import tempfile
import unittest
from concurrent.futures import ThreadPoolExecutor

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "lib"))
import oolib


class DeploymentTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir=os.environ.get("TMPDIR"))
        self.addCleanup(self.directory.cleanup)
        self.path = Path(self.directory.name) / "stats.image"

    def test_legacy_upgrade_idempotent_redeploy_and_rollback_advance_versions(self):
        self.path.write_text("stats:old\n")
        self.assertEqual(oolib.read_app_deployment(self.path), oolib.AppDeployment("stats:old", 0))
        self.assertEqual(oolib.set_app_deployment(self.path, "stats:old").version, 1)
        self.assertEqual(oolib.set_app_deployment(self.path, "stats:old").version, 1)
        self.assertEqual(oolib.set_app_deployment(self.path, "stats:new").version, 2)
        previous = oolib.read_app_deployment(self.path.with_suffix(".image.prev"))
        self.assertEqual(previous, oolib.AppDeployment("stats:old", 1))
        self.assertEqual(oolib.set_app_deployment(self.path, previous.image).version, 3)
        self.assertEqual(self.path.stat().st_mode & 0o777, 0o600)

    def test_concurrent_deployments_get_distinct_versions(self):
        with ThreadPoolExecutor(max_workers=8) as pool:
            results = list(pool.map(lambda n: oolib.set_app_deployment(self.path, f"stats:{n}"), range(16)))
        self.assertEqual(sorted(x.version for x in results), list(range(1, 17)))
        self.assertEqual(oolib.read_app_deployment(self.path).version, 16)

    def test_malformed_and_unknown_records_never_select_an_image(self):
        for value in ["{", '{"schema":2,"image":"stats:a","version":1}',
                      '{"schema":1,"image":"stats:a","version":true}',
                      '{"schema":1,"image":"stats:a","version":-1}',
                      '{"schema":1,"image":"stats:a","version":1,"other":0}', ""]:
            self.path.write_text(value)
            with self.assertRaises(oolib.ConfigError):
                oolib.read_app_deployment(self.path)

    def test_egress_is_explicit_and_does_not_publish_private_ports(self):
        cfg = oolib.load()
        cfg.stack["etc_dir"] = self.directory.name
        app = replace(cfg.apps["discover"], internet=False)
        cfg.apps["discover"] = app
        cfg.app_image_file(app).parent.mkdir()
        oolib.set_app_deployment(cfg.app_image_file(app), "discover:verified")
        private = oolib.run_args(cfg, "app", "discover")
        self.assertNotIn(cfg.egress_network, private)
        cfg.apps["discover"] = replace(app, internet=True)
        egress = oolib.run_args(cfg, "app", "discover")
        self.assertIn(f"name={cfg.network['name']},ip={app.ip}", egress)
        self.assertIn(cfg.egress_network, egress)
        for forbidden in ["--publish", "-p", "--publish-all", "--privileged"]:
            self.assertNotIn(forbidden, egress)
        self.assertFalse(cfg.apps["stats"].internet)
        self.assertFalse(cfg.apps["stuff"].internet)
        cfg.raw["apps"]["discover"]["internet"] = "true"
        with self.assertRaises(oolib.ConfigError):
            oolib.validate(cfg.raw)

    def test_worker_vars_and_secret_names_have_one_derived_binding_list(self):
        cfg = oolib.load()
        cfg.stack["etc_dir"] = self.directory.name
        app = replace(cfg.apps["discover"], vars={"SPACE_NAME": "replacement-rehearsal"})
        cfg.apps["discover"] = app
        commands = runpy.run_path(str(Path(__file__).resolve().parents[1] / "bin" / "oo"))
        commands["cmd_secrets"](cfg)
        values = oolib.read_env(cfg.app_env(app))
        self.assertEqual(values["SPACE_NAME"], "replacement-rehearsal")
        self.assertIn("SPACE_NAME", values["ORANGE_WORKER_SECRETS"].split(","))
        for invalid in [{"S3_SECRET_ACCESS_KEY": "overridden"}, {"OPERATOR_BOOTSTRAP_SHA256": "overlap"}, {"SPACE_NAME": "line\nbreak"}]:
            cfg.raw["apps"]["discover"]["vars"] = invalid
            with self.assertRaises(oolib.ConfigError):
                oolib.validate(cfg.raw)

    def test_app_arguments_use_deployment_version_and_image_together(self):
        cfg = oolib.load()
        cfg.stack["etc_dir"] = self.directory.name
        app = cfg.apps["stats"]
        cfg.app_image_file(app).parent.mkdir()
        oolib.set_app_deployment(cfg.app_image_file(app), "stats:verified")
        args = oolib.run_args(cfg, "app", "stats")
        self.assertIn("RIVET_NODE_VERSION=1", args)
        self.assertEqual(args[-1], "stats:verified")


if __name__ == "__main__":
    unittest.main()
