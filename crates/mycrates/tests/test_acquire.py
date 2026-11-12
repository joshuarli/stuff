import threading
import time

from mycrates.acquire import fetch, plan
from mycrates.config import Config
from mycrates.errors import InputError, IntegrityError, LimitError
from mycrates.store import Store

from .support import FakeUpstream, ScratchTestCase, cli, init_state, lockfile_text, make_crate, sha256


class AcquireTest(ScratchTestCase):
    def setup_method(self, method=None):
        super().setup_method(method)
        self.upstream = FakeUpstream()
        self.addCleanup(self.upstream.close)
        self.state, self.site = init_state(self.tmp)
        self.configure(index_url=self.upstream.url)

    def configure(self, **changes):
        config = Config.load(self.state / "config.json")
        for key, value in changes.items():
            target = config.upstream if key == "index_url" else config.limits
            setattr(target, key, value)
        config.save(self.state / "config.json")
        return config

    def pin(self, *crates, project="proj"):
        lock = self.tmp / f"{project}.lock"
        lock.write_text(lockfile_text(crates))
        code, _, err = cli("import-lock", lock, "--project", project, "--state-dir", self.state)
        self.assertEqual(code, 0, err)

    def config(self):
        return Config.load(self.state / "config.json")

    def test_fetch_downloads_verifies_and_is_idempotent(self):
        checksum = self.upstream.add("alpha", "1.0.0")
        self.pin(("alpha", "1.0.0", checksum))
        self.assertEqual([i.name for i in plan(self.state)], ["alpha"])
        summary = fetch(self.state, self.config())
        self.assertEqual(summary.fetched_count, 1)
        self.assertGreater(summary.total_bytes, 0)
        self.assertEqual(plan(self.state), [])
        requests = len(self.upstream.requests)
        self.assertEqual(fetch(self.state, self.config()).fetched_count, 0)
        self.assertEqual(len(self.upstream.requests), requests)
        Store(self.state).verify()
        record = Store(self.state).load_upstream_records()[0]
        self.assertEqual((record.name, record.vers, record.checksum), ("alpha", "1.0.0", checksum))
        self.assertGreater(record.acquired_at, 0)
        self.assertIn('"cksum":"' + checksum, record.raw_index_line)

    def test_a_lockfile_checksum_that_the_index_contradicts_is_a_trust_chain_violation(self):
        checksum = self.upstream.add("alpha", "1.0.0")
        self.pin(("alpha", "1.0.0", "0" * 64))
        with self.assertRaisesRegex(IntegrityError, "trust chain violation"):
            fetch(self.state, self.config())
        self.assertEqual(list(Store(self.state).blobs_dir.glob("*.crate")), [])
        self.assertEqual(checksum, self.upstream.entry_line("alpha", "1.0.0").split('"cksum":"')[1][:64])

    def test_downloaded_bytes_must_match_the_index_checksum(self):
        checksum = self.upstream.add("alpha", "1.0.0")
        self.upstream.download_overrides[("alpha", "1.0.0")] = b"substituted"
        self.pin(("alpha", "1.0.0", checksum))
        with self.assertRaisesRegex(IntegrityError, "download checksum mismatch"):
            fetch(self.state, self.config())
        self.assertEqual(list(Store(self.state).blobs_dir.glob("*.crate")), [])
        self.assertEqual(Store(self.state).load_upstream_records(), [])

    def test_an_archive_that_is_really_another_crate_is_rejected_even_with_a_matching_checksum(self):
        impostor = make_crate("other", "1.0.0")
        self.upstream.crates[("alpha", "1.0.0")] = impostor
        self.pin(("alpha", "1.0.0", sha256(impostor)))
        with self.assertRaises(IntegrityError):
            fetch(self.state, self.config())
        self.assertEqual(Store(self.state).load_upstream_records(), [])

    def test_a_version_missing_from_the_index_fails_closed(self):
        self.upstream.add("alpha", "1.0.0")
        self.pin(("alpha", "9.9.9", "a" * 64))
        with self.assertRaisesRegex(InputError, "not found in upstream index"):
            fetch(self.state, self.config())

    def test_the_byte_budget_stops_the_run(self):
        checksum = self.upstream.add("alpha", "1.0.0", files={"payload": b"x" * 4000})
        self.pin(("alpha", "1.0.0", checksum))
        self.configure(fetch_bytes=10)
        with self.assertRaises(LimitError):
            fetch(self.state, self.config())

    def test_the_fetch_deadline_stops_the_run(self):
        checksum = self.upstream.add("alpha", "1.0.0")
        self.pin(("alpha", "1.0.0", checksum))
        self.configure(fetch_timeout_secs=0)
        with self.assertRaisesRegex(LimitError, "fetch_timeout_secs"):
            fetch(self.state, self.config())

    def test_oversized_downloads_are_cut_off(self):
        checksum = self.upstream.add("alpha", "1.0.0", files={"payload": b"x" * 5000})
        self.pin(("alpha", "1.0.0", checksum))
        self.configure(max_compressed=200)
        with self.assertRaises(LimitError):
            fetch(self.state, self.config())

    def test_many_pins_download_concurrently_within_the_configured_bound(self):
        pins = [(f"crate{i}", "1.0.0", self.upstream.add(f"crate{i}", "1.0.0")) for i in range(12)]
        self.pin(*pins)
        active = peak = 0
        mutex = threading.Lock()

        def hook(name, version):
            nonlocal active, peak
            with mutex:
                active += 1
                peak = max(peak, active)
            time.sleep(0.05)
            with mutex:
                active -= 1

        self.upstream.download_hook = hook
        self.configure(concurrent_requests=3)
        self.assertEqual(fetch(self.state, self.config()).fetched_count, 12)
        self.assertGreater(peak, 1)
        self.assertLessEqual(peak, 3)
        Store(self.state).verify()

    def test_the_store_lock_is_free_while_a_download_is_in_flight(self):
        checksum = self.upstream.add("alpha", "1.0.0")
        self.pin(("alpha", "1.0.0", checksum))
        entered, release = threading.Event(), threading.Event()

        def hook(name, version):
            entered.set()
            release.wait(10)

        self.upstream.download_hook = hook
        worker = threading.Thread(target=lambda: fetch(self.state, self.config()))
        worker.start()
        self.assertTrue(entered.wait(10))
        started = time.monotonic()
        with Store(self.state).lock_exclusive():
            pass
        self.assertLess(time.monotonic() - started, 2)
        release.set()
        worker.join(30)
        self.assertEqual(len(Store(self.state).load_upstream_records()), 1)

    def test_add_pins_the_checksum_the_configured_index_reports(self):
        checksum = self.upstream.add("alpha", "1.0.0")
        code, out, err = cli("add", "alpha@1.0.0", "--state-dir", self.state)
        self.assertEqual((code, out.strip()), (0, "added alpha@1.0.0"), err)
        again = cli("add", "alpha@1.0.0", "--state-dir", self.state)
        self.assertEqual(again[1].strip(), "already pinned alpha@1.0.0")
        self.assertEqual([i.checksum for i in plan(self.state)], [checksum])
        self.assertEqual(cli("add", "alpha@9.9.9", "--state-dir", self.state)[0], 2)
        self.assertEqual(cli("add", "alpha", "--state-dir", self.state)[0], 2)

    def test_untrusted_urls_are_refused(self):
        for url in ["http://example.com/", "ftp://example.com/", "https://user:pw@example.com/", "https:///nohost", "not a url"]:
            with self.subTest(url=url):
                self.configure(index_url=url)
                self.pin(("alpha", "1.0.0", "a" * 64))
                with self.assertRaises(InputError):
                    fetch(self.state, self.config())

    def test_download_template_placeholders_expand_and_unknown_ones_are_rejected(self):
        from mycrates.config import LimitsConfig
        from mycrates.upstream import UpstreamClient

        client = UpstreamClient(self.upstream.url, LimitsConfig())
        expand = client.expand_dl_url
        self.assertEqual(
            expand("https://dl.test/{lowerprefix}/{crate}/{version}/{sha256-checksum}", "Serde", "1.0.0", "ab"),
            "https://dl.test/se/rd/Serde/1.0.0/ab",
        )
        self.assertEqual(expand("https://dl.test/api/", "serde", "1.0.0", "ab"), "https://dl.test/api/serde/1.0.0/download")
        with self.assertRaisesRegex(InputError, "unsupported placeholder"):
            expand("https://dl.test/{crate}/{unknown}", "serde", "1.0.0", "ab")
        with self.assertRaises(InputError):
            expand("http://dl.test/{crate}", "serde", "1.0.0", "ab")

    def test_redirects_are_followed_within_the_limit_and_must_stay_secure(self):
        import http.server

        class Redirect(http.server.BaseHTTPRequestHandler):
            def do_GET(inner):
                inner.send_response(302)
                inner.send_header("Location", inner.server.target)
                inner.send_header("Content-Length", "0")
                inner.end_headers()

            def log_message(inner, *args):
                pass

        httpd = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Redirect)
        threading.Thread(target=httpd.serve_forever, kwargs={"poll_interval": 0.02}, daemon=True).start()
        self.addCleanup(httpd.server_close)
        self.addCleanup(httpd.shutdown)
        from mycrates.config import LimitsConfig
        from mycrates.errors import NetworkError
        from mycrates.upstream import UpstreamClient

        base = f"http://127.0.0.1:{httpd.server_address[1]}/"
        client = UpstreamClient(base, LimitsConfig(redirects=2))
        httpd.target = base
        with self.assertRaisesRegex(NetworkError, "too many redirects"):
            client.get(base)
        httpd.target = "http://example.com/insecure"
        with self.assertRaises(InputError):
            client.get(base)
        httpd.target = self.upstream.url + "config.json"
        self.assertIn(b"dl", client.get(base))


