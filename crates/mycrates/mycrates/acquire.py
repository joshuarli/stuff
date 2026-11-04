import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass

from .errors import Error, IntegrityError, LimitError
from .pins import read_pins
from .store import Store, UpstreamRecord
from .upstream import UpstreamClient


@dataclass(frozen=True, order=True)
class PlanItem:
    name: str
    version: str
    checksum: str


@dataclass
class FetchSummary:
    fetched_count: int = 0
    total_bytes: int = 0


def plan(state_dir):
    store = Store(state_dir)
    store.assert_current_format()
    needed = []
    if not store.pins_dir.exists():
        return needed
    for path in sorted(store.pins_dir.glob("*.json")):
        for pin in read_pins(path).registry:
            record = store.upstream_dir / pin.name.lower() / f"{pin.version}.json"
            if not record.exists() or not store.blob_path(pin.checksum).exists():
                item = PlanItem(pin.name, pin.version, pin.checksum)
                if item not in needed:
                    needed.append(item)
    needed.sort(key=lambda item: (item.name, item.version))
    return needed


def fetch(state_dir, config):
    store = Store(state_dir)
    items = plan(state_dir)
    if not items:
        return FetchSummary()
    store.ensure_dirs()
    client = UpstreamClient(config.upstream.index_url, config.limits, spill_dir=store.uploads_dir)
    template = client.fetch_dl_template()
    deadline = time.monotonic() + config.limits.fetch_timeout_secs
    budget = [config.limits.fetch_bytes]
    summary = FetchSummary()
    mutex = threading.Lock()
    aborted = threading.Event()
    first_error = []

    def work(item):
        if aborted.is_set():
            return
        try:
            if time.monotonic() >= deadline:
                raise LimitError("fetch_timeout_secs exceeded")
            _fetch_item(store, client, template, item, mutex, budget, summary)
        except Error as error:
            aborted.set()
            with mutex:
                first_error.append(error)

    workers = max(1, min(config.limits.concurrent_requests, len(items)))
    with ThreadPoolExecutor(max_workers=workers) as pool:
        list(pool.map(work, items))
    if first_error:
        raise first_error[0]
    return summary


def _fetch_item(store, client, template, item, mutex, budget, summary):
    raw_line, entry = client.fetch_index_entry(item.name, item.version)
    if entry.cksum != item.checksum:
        raise IntegrityError(
            f"trust chain violation for {item.name} {item.version}: "
            f"pin checksum {item.checksum} != index cksum {entry.cksum}"
        )
    url = client.expand_dl_url(template, item.name, item.version, item.checksum)
    staged = client.download_crate(url, item.checksum, item.name, item.version)
    try:
        size = staged.seek(0, 2)
        with mutex:
            if size > budget[0]:
                raise LimitError("fetch byte budget exceeded")
            budget[0] -= size
            summary.total_bytes += size
        record = UpstreamRecord(item.name, item.version, item.checksum, raw_line, int(time.time()))
        with store.lock_exclusive():
            staged.seek(0)
            if store.ingest_blob(staged) != item.checksum:
                raise IntegrityError("ingested checksum mismatch")
            store.put_upstream_record(record)
    finally:
        staged.close()
    with mutex:
        summary.fetched_count += 1


def verify(state_dir):
    Store(state_dir).verify()
