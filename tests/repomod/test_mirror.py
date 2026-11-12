from __future__ import annotations

import subprocess
from collections.abc import Sequence
from pathlib import Path

import pytest

from repomod import audit, mirror
from repomod.dependencies.model import Ecosystem
from repomod.mirror import Package
from repomod.rules.registry_sources import RegistryPolicy
from repomod.types import Catalog, RepoId, Repository

CARGO = "sparse+https://crates.example/current/crates-io/"
NPM = "https://npm.example/current/"
PYPI = "https://pypi.example/current/simple/"
POLICY = RegistryPolicy(True, CARGO, "sparse+https://crates.example/current/registry/", NPM, PYPI, ())
TOOLS = {"uv": "0.13.7", "ruff": "0.17.4", "ty": "0.0.99"}

CARGO_LOCK = """version = 4

[[package]]
name = "demo"
version = "0.1.0"

[[package]]
name = "rustybench"
version = "{rustybench}"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{checksum}"

[[package]]
name = "laputa-ptytest"
version = "0.3.0"
source = "sparse+https://crates.example/current/registry/"
checksum = "bb"

[[package]]
name = "forked"
version = "1.0.0"
source = "git+https://github.com/acme/forked?rev=abc#abc"
"""

UV_LOCK = """version = 1

[[package]]
name = "demo"
version = "1.0.0"
source = {{ editable = "." }}

[[package]]
name = "ty"
version = "0.0.99"
source = {{ registry = "{index}" }}

[[package]]
name = "Requests"
version = "{requests}"
source = {{ registry = "{index}" }}
"""


@pytest.fixture(autouse=True)
def isolated(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(audit, "_audit_cache_path", lambda: tmp_path / "audit.json")
    monkeypatch.setattr(audit, "_http_json", lambda url, headers=None: None)
    monkeypatch.setattr(audit, "_crates_fetch", lambda url: None)
    monkeypatch.setattr(mirror, "load_approved_python_tool_versions", lambda: TOOLS)
    monkeypatch.setattr("repomod.rules.audit_upgrades.shutil.which", lambda tool: f"/usr/bin/{tool}")


def _repository(
    name: str = "demo", *tags: str, private: bool = False, ignored_rules: frozenset[str] = frozenset()
) -> Repository:
    return Repository(RepoId("acme", name), ("rust", "python", *tags), ignored_rules=ignored_rules, private=private)


def _checkout(tmp_path: Path, name: str = "demo") -> Path:
    root = tmp_path / "repos" / name
    (root / ".cargo").mkdir(parents=True)
    (root / ".cargo" / "config.toml").write_text(
        '[source.crates-io]\nreplace-with = "stuff"\n\n'
        f'[source.stuff]\nregistry = "{CARGO}"\n\n'
        '[registries.stuff-native]\nindex = "sparse+https://crates.example/current/registry/"\n',
        encoding="utf-8",
    )
    (root / "Cargo.toml").write_text(
        '[package]\nname = "demo"\nversion = "0.1.0"\n\n[dependencies]\nrustybench = "0.4"\n', encoding="utf-8"
    )
    (root / "Cargo.lock").write_text(CARGO_LOCK.format(rustybench="0.4.0", checksum="aa"), encoding="utf-8")
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\ndependencies = ["requests>=2"]\n\n'
        f'[[tool.uv.index]]\nurl = "{PYPI}"\ndefault = true\n',
        encoding="utf-8",
    )
    (root / "uv.lock").write_text(UV_LOCK.format(index=PYPI, requests="2.32.0"), encoding="utf-8")
    return root


def _catalog(tmp_path: Path, *repositories: Repository) -> Catalog:
    return Catalog(tmp_path / "catalog.toml", tmp_path / "repos", "acme", repositories)


def _upstream_relock(monkeypatch: pytest.MonkeyPatch, calls: list[list[str]]) -> None:
    """Fake upgraders that bump rustybench to 0.5.0 and requests to 2.33.0, as upstream would."""

    def run(argv: list[str], *, cwd: Path, **_: object) -> subprocess.CompletedProcess[str]:
        work = Path(cwd)
        calls.append(argv)
        if argv[0] == "cargo":
            config = work / ".cargo" / "config.toml"
            assert not config.exists() or "[source" not in config.read_text()
            assert 'rustybench = "0.5"' in (work / "Cargo.toml").read_text()
            (work / "Cargo.lock").write_text(CARGO_LOCK.format(rustybench="0.5.0", checksum="cc"))
        elif argv[0] == "uv":
            (work / "uv.lock").write_text(UV_LOCK.format(index="https://pypi.org/simple", requests="2.33.0"))
        return subprocess.CompletedProcess(argv, 0, "", "")

    monkeypatch.setattr("repomod.rules.audit_upgrades.subprocess.run", run)
    monkeypatch.setattr(
        "repomod.rules.audit_upgrades.resolve_latest",
        lambda items, **kwargs: {("crates", "rustybench"): "0.5.0" if kwargs.get("sources") is None else "0.4.0"},
    )


class Stuff:
    """Records `stuff publish` calls and serves what they publish."""

    def __init__(self, served: dict[Ecosystem, dict[str, frozenset[str]]], failing: Sequence[str] = ()) -> None:
        self.served = served
        self.failing = tuple(failing)
        self.calls: list[list[str]] = []

    def read(self, _: RegistryPolicy) -> dict[Ecosystem, dict[str, frozenset[str]]]:
        return self.served

    def run(self, argv: Sequence[str]) -> subprocess.CompletedProcess[str]:
        self.calls.append(list(argv))
        if any(fragment in " ".join(argv) for fragment in self.failing):
            return subprocess.CompletedProcess(list(argv), 4, "", "integrity: checksum mismatch\n")
        ecosystem = Ecosystem(argv[2])
        if ecosystem is Ecosystem.PYPI:
            packages = [Package(ecosystem, *spec.split("==")) for spec in argv[3:]]
        else:
            locks = [Path(argv[index + 1]) for index, value in enumerate(argv) if value == "--lockfile"]
            packages = [package for lock in locks for package in mirror._parse_lockfile(lock, POLICY)]
        for package in packages:
            name, version = mirror._key(package)
            table = self.served.setdefault(ecosystem, {})
            table[name] = table.get(name, frozenset()) | {version}
        return subprocess.CompletedProcess(list(argv), 0, "", "")


def _served() -> dict[Ecosystem, dict[str, frozenset[str]]]:
    return {
        Ecosystem.CARGO: {"rustybench": frozenset({"0.4.0"})},
        Ecosystem.NPM: {},
        Ecosystem.PYPI: {
            "requests": frozenset({"2.32.0"}),
            "ty": frozenset({"0.0.99"}),
            "uv": frozenset({"0.13.7"}),
            "ruff": frozenset({"0.17.4"}),
        },
    }


def _no_wait(missing, read):
    served = read()
    return {package for package in missing if not mirror.is_served(package, served)}


def test_cargo_lock_keeps_only_crates_io_packages() -> None:
    packages = mirror.parse_cargo_lock(CARGO_LOCK.format(rustybench="0.4.0", checksum="aa"))
    assert packages == {Package(Ecosystem.CARGO, "rustybench", "0.4.0")}


def test_uv_lock_keeps_only_registry_packages() -> None:
    packages = mirror.parse_uv_lock(UV_LOCK.format(index=PYPI, requests="2.32.0"))
    assert packages == {Package(Ecosystem.PYPI, "ty", "0.0.99"), Package(Ecosystem.PYPI, "requests", "2.32.0")}


def test_pnpm_lock_keeps_registry_tarballs_and_skips_other_sources() -> None:
    text = f"""lockfileVersion: '9.0'
packages:
  '@scope/pkg@1.2.3':
    resolution: {{integrity: sha512-a}}
  plain@4.0.0:
    resolution: {{integrity: sha512-b, tarball: {NPM}plain/-/plain-4.0.0.tgz}}
  fromgit@1.0.0:
    resolution: {{integrity: sha512-c, tarball: https://codeload.github.com/acme/fromgit/tar.gz/abc}}
  local@0.0.1:
    resolution: {{directory: ../local, type: directory}}
"""
    assert mirror.parse_pnpm_lock(text, (NPM,)) == {
        Package(Ecosystem.NPM, "@scope/pkg", "1.2.3"),
        Package(Ecosystem.NPM, "plain", "4.0.0"),
    }


def test_mirror_publishes_upgrade_targets_missing_on_stuff_and_clears_stale_latest(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = _checkout(tmp_path)
    repository = _repository()
    relocks: list[list[str]] = []
    _upstream_relock(monkeypatch, relocks)
    with audit.audit_cache() as cache:
        cache["latest"] = {
            f"crates:{CARGO}:rustybench:0.4.0": {"version": "0.4.0", "fetched_at": 1e12},
            "crates:rustybench:0.4.0": {"version": "0.5.0", "fetched_at": 1e12},
        }
    stuff = Stuff(_served())
    report = mirror.mirror(
        _catalog(tmp_path, repository),
        [repository],
        dry_run=False,
        runner=stuff.run,
        policy=POLICY,
        read=stuff.read,
        wait=_no_wait,
    )
    assert report.failures == []
    assert report.published == {
        Package(Ecosystem.CARGO, "rustybench", "0.5.0"),
        Package(Ecosystem.PYPI, "requests", "2.33.0"),
    }
    assert [call[:3] for call in relocks] == [["uv", "lock", "--upgrade"], ["cargo", "update"]]
    assert relocks[0][3:] == ["--default-index", "https://pypi.org/simple"]
    cargo_calls = [call for call in stuff.calls if call[2] == "cargo"]
    assert len(cargo_calls) == 1
    assert cargo_calls[0][3] == "--lockfile"
    assert Path(cargo_calls[0][4]).parent.name == "demo"
    assert [call for call in stuff.calls if call[2] == "pypi"] == [["stuff", "publish", "pypi", "requests==2.33.0"]]
    assert 'rustybench = "0.4"' in (root / "Cargo.toml").read_text()
    with audit.audit_cache() as cache:
        assert list(cache["latest"]) == ["crates:rustybench:0.4.0"]


def test_mirror_does_nothing_when_stuff_serves_everything(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _checkout(tmp_path)
    repository = _repository()
    _upstream_relock(monkeypatch, [])
    served = _served()
    served[Ecosystem.CARGO]["rustybench"] = frozenset({"0.4.0", "0.5.0"})
    served[Ecosystem.PYPI]["requests"] = frozenset({"2.32.0", "2.33.0"})
    stuff = Stuff(served)
    report = mirror.mirror(
        _catalog(tmp_path, repository), [repository], dry_run=False, runner=stuff.run, policy=POLICY, read=stuff.read
    )
    assert (report.published, report.failures, stuff.calls) == (set(), [], [])


def test_dry_run_lists_missing_versions_without_publishing(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    _checkout(tmp_path)
    repository = _repository()
    _upstream_relock(monkeypatch, [])
    stuff = Stuff(_served())
    plans: list[mirror.Plan] = []
    mirror.mirror(
        _catalog(tmp_path, repository),
        [repository],
        dry_run=True,
        runner=stuff.run,
        policy=POLICY,
        read=stuff.read,
        on_plan=plans.append,
    )
    assert stuff.calls == []
    assert mirror.summary(plans[0]) == [
        "cargo: 1 missing on stuff",
        "  rustybench 0.5.0",
        "pypi: 1 missing on stuff",
        "  requests 2.33.0",
    ]


def test_failed_batch_is_retried_per_lockfile_to_name_the_broken_one(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    for name in ("one", "two"):
        _checkout(tmp_path, name)
    repositories = [_repository("one"), _repository("two")]
    _upstream_relock(monkeypatch, [])
    stuff = Stuff(_served())
    monkeypatch.setattr(mirror, "_project_id", lambda repository, *_: f"acme-{repository.id.name}")
    plan = mirror.plan(_catalog(tmp_path, *repositories), repositories, POLICY, _served(), tmp_path / "scratch")
    stuff.failing = tuple(
        str(lock.path) for lock in plan.lockfiles if "/two" in str(lock.path) and lock.ecosystem is Ecosystem.CARGO
    )
    failures = mirror.publish(plan, stuff.run)
    cargo_calls = [call for call in stuff.calls if call[2] == "cargo"]
    assert len(cargo_calls) == 1 + 2
    assert len(cargo_calls[0]) == 3 + 2 * 2
    assert failures == ["cargo acme-two: integrity: checksum mismatch"]


def test_private_forked_and_ignoring_repositories_are_not_mirrored() -> None:
    kept = _repository("kept")
    assert mirror.eligible(
        [
            kept,
            _repository("private", private=True),
            _repository("fork", "fork"),
            _repository("ignoring", ignored_rules=frozenset({"audit-upgrades"})),
        ]
    ) == [kept]


def test_repository_on_another_registry_is_not_mirrored(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    root = _checkout(tmp_path)
    (root / ".cargo" / "config.toml").write_text(
        '[source.crates-io]\nreplace-with = "work"\n\n[source.work]\nregistry = "sparse+https://work.example/"\n'
    )
    (root / "pyproject.toml").write_text(
        '[project]\nname = "demo"\nversion = "1.0.0"\n\n'
        '[[tool.uv.index]]\nurl = "https://work.example/simple/"\ndefault = true\n',
        encoding="utf-8",
    )
    repository = _repository()
    _upstream_relock(monkeypatch, [])
    plan = mirror.plan(_catalog(tmp_path, repository), [repository], POLICY, _served(), tmp_path / "scratch")
    assert (plan.lockfiles, plan.missing, plan.failures) == ([], set(), [])


def test_a_repository_still_on_public_registries_is_mirrored_before_it_moves_to_stuff(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    root = _checkout(tmp_path)
    (root / ".cargo" / "config.toml").unlink()
    repository = _repository()
    _upstream_relock(monkeypatch, [])
    plan = mirror.plan(_catalog(tmp_path, repository), [repository], POLICY, _served(), tmp_path / "scratch")
    assert Package(Ecosystem.CARGO, "rustybench", "0.5.0") in plan.missing
    exempt = _repository(ignored_rules=frozenset({"registry-sources"}))
    plan = mirror.plan(_catalog(tmp_path, exempt), [exempt], POLICY, _served(), tmp_path / "scratch-exempt")
    assert {package.ecosystem for package in plan.missing} == {Ecosystem.PYPI}


def test_approved_python_tools_are_mirrored_with_python_lockfiles(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _checkout(tmp_path)
    repository = _repository()
    _upstream_relock(monkeypatch, [])
    served = _served()
    del served[Ecosystem.PYPI]["ruff"]
    plan = mirror.plan(_catalog(tmp_path, repository), [repository], POLICY, served, tmp_path / "scratch")
    assert Package(Ecosystem.PYPI, "ruff", "0.17.4") in plan.missing


def test_a_failing_repository_is_reported_and_the_rest_still_plan(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _checkout(tmp_path, "good")
    repositories = [_repository("good"), _repository("gone")]
    _upstream_relock(monkeypatch, [])
    plan = mirror.plan(_catalog(tmp_path, *repositories), repositories, POLICY, _served(), tmp_path / "scratch")
    assert plan.failures == ["acme/gone: checkout is missing; run repomod sync"]
    assert Package(Ecosystem.CARGO, "rustybench", "0.5.0") in plan.missing


def test_wait_until_served_polls_until_the_catalog_lists_every_package() -> None:
    package = Package(Ecosystem.CARGO, "rustybench", "0.5.0")
    reads = iter([{}, {}, {Ecosystem.CARGO: {"rustybench": frozenset({"0.5.0"})}}])
    now = [0.0]
    pending = mirror.wait_until_served(
        [package],
        lambda: next(reads),
        timeout=100,
        interval=10,
        clock=lambda: now[0],
        sleep=lambda s: now.__setitem__(0, now[0] + s),
    )
    assert pending == set()
    assert now[0] == 20


def test_wait_until_served_gives_up_at_the_deadline() -> None:
    package = Package(Ecosystem.NPM, "left", "1.0.0")
    now = [0.0]
    pending = mirror.wait_until_served(
        [package], dict, timeout=30, interval=10, clock=lambda: now[0], sleep=lambda s: now.__setitem__(0, now[0] + s)
    )
    assert pending == {package}


@pytest.mark.parametrize(
    ("rules", "no_mirror", "mirrored"),
    [
        (["audit-upgrades"], False, True),
        (["python-standard"], False, True),
        (["audit-upgrades"], True, False),
        (["gha-syntax"], False, False),
    ],
)
def test_fix_mirrors_before_upgrading_unless_told_not_to(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, rules: list[str], no_mirror: bool, mirrored: bool
) -> None:
    import argparse

    from repomod.cmd import fix as fix_command
    from repomod.fix import FixOutcome
    from repomod.github import RemoteRepository

    repository = _repository()
    catalog = _catalog(tmp_path, repository)
    calls: list[tuple[str, ...]] = []
    monkeypatch.setattr("repomod.cmd.common.default_catalog", lambda: catalog)
    monkeypatch.setattr(
        "repomod.github.remote_metadata",
        lambda repositories: {
            str(repo.id): RemoteRepository(str(repo.id), "main", "a" * 40, False) for repo in repositories
        },
    )
    monkeypatch.setattr(fix_command, "sync_repositories", lambda *args, **kwargs: [])
    monkeypatch.setattr(
        "repomod.fix.fix_repository", lambda catalog, repository, *args: FixOutcome(str(repository.id))
    )

    def fake_mirror(catalog, repositories, **kwargs):
        calls.append(tuple(str(repo.id) for repo in repositories))
        return mirror.Report(failures=["cargo acme-demo: integrity: checksum mismatch"])

    monkeypatch.setattr(mirror, "mirror", fake_mirror)
    args = argparse.Namespace(repo=["acme/demo"], tag=[], rule=rules, workers=1, no_mirror=no_mirror)
    status = fix_command._fix(args)
    assert calls == ([("acme/demo",)] if mirrored else [])
    assert status == (1 if mirrored else 0)


def test_pnpm_lock_reads_every_yaml_document() -> None:
    text = """---
lockfileVersion: '9.0'
importers: {}
---
lockfileVersion: '9.0'
packages:
  extra@2.0.0:
    resolution: {integrity: sha512-x}
"""
    assert mirror.parse_pnpm_lock(text, (NPM,)) == {Package(Ecosystem.NPM, "extra", "2.0.0")}


def test_a_failed_upstream_relock_still_mirrors_the_current_lockfiles(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    _checkout(tmp_path)
    repository = _repository()

    def broken(*args, **kwargs):
        raise RuntimeError("cannot preserve Cargo manifest layout: Cargo.toml")

    monkeypatch.setattr("repomod.rules.audit_upgrades.upgrade_in_sandbox", broken)
    served = _served()
    served[Ecosystem.CARGO] = {}
    plan = mirror.plan(_catalog(tmp_path, repository), [repository], POLICY, served, tmp_path / "scratch")
    assert plan.failures == ["acme/demo: upstream relock failed: cannot preserve Cargo manifest layout: Cargo.toml"]
    assert plan.missing == {Package(Ecosystem.CARGO, "rustybench", "0.4.0")}


def test_pnpm_packages_for_platforms_stuff_does_not_mirror_are_skipped() -> None:
    text = """lockfileVersion: '9.0'
packages:
  '@pnpm/exe.android-arm64@12.18.0':
    resolution: {integrity: sha512-a}
    cpu: [arm64]
    os: [android]
  '@pnpm/exe.linux-s390x@12.18.0':
    resolution: {integrity: sha512-b}
    cpu: [s390x]
    os: [linux]
  not-windows@1.0.0:
    resolution: {integrity: sha512-c}
    os: ['!win32']
  only-not-unix@1.0.0:
    resolution: {integrity: sha512-d}
    os: ['!darwin', '!linux']
  '@pnpm/exe.darwin-arm64@12.18.0':
    resolution: {integrity: sha512-e}
    cpu: [arm64]
    os: [darwin]
"""
    assert mirror.parse_pnpm_lock(text, (NPM,)) == {
        Package(Ecosystem.NPM, "not-windows", "1.0.0"),
        Package(Ecosystem.NPM, "@pnpm/exe.darwin-arm64", "12.18.0"),
    }
