import json

from mycrates.config import Config
from mycrates.store import Store

from .support import GOLDEN, ScratchTestCase, cli, http, start_server, wire_payload


class AdoptPublishGoldenTest(ScratchTestCase):
    def test_adopted_site_republishes_the_recorded_generation_byte_for_byte(self):
        golden = GOLDEN / "adopt-publish"
        state, site = self.tmp / "state", self.tmp / "site"
        self.assertEqual(cli("init", "--state-dir", state, "--site-dir", site)[0], 0)
        config = Config.load(golden / "config.json")
        config.save(state / "config.json")
        self.assertEqual(cli("adopt", "--from-site", golden / "input-site", "--state-dir", state)[0], 0)
        self.assertEqual(cli("publish", "--state-dir", state, "--site-dir", site)[0], 0)

        generation_id = (golden / "expected-generation-id").read_text().strip()
        self.assertEqual((site / "current").readlink().as_posix(), f"generations/{generation_id}")
        expected = golden / "expected-generation"
        produced = site / "generations" / generation_id
        expected_files = sorted(p.relative_to(expected) for p in expected.rglob("*") if p.is_file())
        produced_files = sorted(p.relative_to(produced) for p in produced.rglob("*") if p.is_file())
        self.assertEqual(produced_files, expected_files)
        for relative in expected_files:
            self.assertEqual((produced / relative).read_bytes(), (expected / relative).read_bytes(), str(relative))


class PublishGoldenTest(ScratchTestCase):
    cases = sorted(p for p in (GOLDEN / "publish").iterdir() if p.is_dir())

    def test_every_case_matches_the_recorded_response_and_index_line(self):
        self.assertGreaterEqual(len(self.cases), 10)
        for case in self.cases:
            with self.subTest(case=case.name):
                self.run_case(case)

    def run_case(self, case):
        work = self.tmp / case.name
        state, site = work / "state", work / "site"
        cli("init", "--state-dir", state, "--site-dir", site)
        cli("config", "set-public-base-url", "http://127.0.0.1:1", "--state-dir", state)
        cli("publish", "--state-dir", state, "--site-dir", site)
        expected = json.loads((case / "expected.json").read_text())
        metadata = json.loads((case / "metadata.json").read_text())
        crate = (case / "crate.crate").read_bytes()
        running = start_server(state, site, token="golden-token")
        self.addCleanup(running.close)
        status, _, body = http(
            "PUT", f"{running.url}/api/v1/crates/new", wire_payload(metadata, crate), {"Authorization": "golden-token"}
        )
        self.assertEqual(status, expected["status"])
        self.assertEqual(json.loads(body), expected["response"])
        if status == 200:
            line = (site / "current" / "registry" / expected["shard"]).read_text().rstrip("\n")
            self.assertEqual(line, expected["index_line"])
            self.assertEqual(json.loads(line)["cksum"], expected["cksum"])
        else:
            self.assertFalse(list(Store(state).blobs_dir.glob("*.crate")))


