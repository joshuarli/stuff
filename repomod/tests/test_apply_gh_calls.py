"""The API budget of an --apply run: how many calls, batched lookups, and caching.

Runs the real pipeline (git against local bare origins) with a fake `gh` that
records every call, so a run's call count is asserted exactly.
"""

import subprocess
from itertools import pairwise
from pathlib import Path

import pytest
from gh_harness import (
    FakeGh,
    branch_pushed,
    make_repo_with_origin,
    node_id,
    offline_clone,
    search_item,
)
from test_main import REPO_A, REPO_B, REPO_C, _mock_sync, _write_fix

from repomod import git
from repomod import main as run_fix
from repomod.status import load_status
from repomod.utils import pr_create_limiter

BRANCH = "test-branch"  # from tests' CONFIG_YAML


def _setup(tmp_path: Path, config_extra: str = "") -> tuple[Path, dict[str, Path]]:
    """A fix plus three repos (alpha, beta, gamma) with local bare origins."""
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    if config_extra:
        with open(fix_dir / "config.yaml", "a") as f:
            f.write(config_extra)
    origins: dict[str, Path] = {}
    for repo in (REPO_A, REPO_B, REPO_C):
        clone, bare = make_repo_with_origin(tmp_path, repo.slug, repo.name)
        (fix_dir / "repos").mkdir(exist_ok=True)
        clone.rename(fix_dir / "repos" / repo.slug)
        origins[repo.name] = bare
    return fix_dir, origins


def test_existing_prs_are_looked_up_once_not_per_repo(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """alpha has no PR, beta has an open one, gamma's was merged.

    Two searches answer for all three, so beta and gamma cost zero API calls:
    beta is just force-pushed, gamma is left alone entirely. The count stays
    two however many repos are in the run.
    """
    fix_dir, origins = _setup(tmp_path)
    fake_gh.configure(
        search_items=[
            search_item("acme/repo-beta", 7, "open"),
            search_item(
                "acme/repo-gamma", 9, "closed", merged_at="2026-09-01T00:00:00Z"
            ),
        ]
    )

    with _mock_sync(repos=[REPO_A, REPO_B, REPO_C]), offline_clone():
        assert run_fix.main([str(fix_dir), "--apply", "--workers", "1"]) == 0

    assert fake_gh.count("api", "--method") == 2  # opening search, then the refresh
    assert fake_gh.count("api", "graphql") == 1  # alpha's new id, batched
    assert fake_gh.calls_for("acme/repo-beta") == []
    assert fake_gh.calls_for("acme/repo-gamma") == []
    assert fake_gh.count("pr", "create", repo="acme/repo-alpha") == 1
    assert fake_gh.count("pr", "view") == 0

    assert branch_pushed(origins["acme/repo-alpha"], BRANCH)
    assert branch_pushed(origins["acme/repo-beta"], BRANCH)  # open PR: updated
    assert not branch_pushed(origins["acme/repo-gamma"], BRANCH)  # merged: untouched

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-alpha"]["status"] == "pr_created"
    assert status["acme/repo-beta"]["status"] == "pr_created"
    assert (
        status["acme/repo-beta"]["pr_url"] == "https://github.com/acme/repo-beta/pull/7"
    )
    assert status["acme/repo-gamma"]["status"] == "pr_merged"
    assert (
        status["acme/repo-gamma"]["pr_url"]
        == "https://github.com/acme/repo-gamma/pull/9"
    )
    assert "pushed_sha" not in status["acme/repo-gamma"]
    for name in ("acme/repo-alpha", "acme/repo-beta"):
        tip = subprocess.run(
            ["git", "rev-parse", BRANCH],
            cwd=origins[name],
            capture_output=True,
            text=True,
            check=True,
        ).stdout.strip()
        assert status[name]["pushed_sha"] == tip


def test_pr_merged_during_the_fix_is_not_force_pushed(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """The refresh is after the fix and before the push.

    The fix rewrites the search snapshot from OPEN to MERGED. A second search
    that ran before the fix would still see OPEN and force-push.
    """
    fix_dir, origins = _setup(tmp_path)
    _write_fix(
        fix_dir,
        body=(
            "    from gh_harness import FakeGh, search_item\n"
            f"    FakeGh(Path({str(fake_gh.root)!r})).configure(\n"
            "        search_items=[\n"
            "            search_item(\n"
            '                "acme/repo-beta",\n'
            "                7,\n"
            '                "closed",\n'
            '                merged_at="2026-10-02T00:00:00Z",\n'
            "            )\n"
            "        ]\n"
            "    )\n"
            "    (repo / 'README.md').write_text('changed')\n"
            "    return True"
        ),
    )
    fake_gh.configure(search_items=[search_item("acme/repo-beta", 7, "open")])

    with _mock_sync(repos=[REPO_B]), offline_clone():
        assert run_fix.main([str(fix_dir), "--apply", "--workers", "1"]) == 0

    assert fake_gh.count("api", "--method") == 2
    assert fake_gh.calls_for("acme/repo-beta") == []
    assert fake_gh.count("pr", "create") == 0
    assert fake_gh.count("pr", "view") == 0
    assert not branch_pushed(origins["acme/repo-beta"], BRANCH)
    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-beta"]["status"] == "pr_merged"
    assert (
        status["acme/repo-beta"]["pr_url"]
        == "https://github.com/acme/repo-beta/pull/7"
    )
    assert "pushed_sha" not in status["acme/repo-beta"]
    assert status["acme/repo-beta"]["pr_node_id"] == node_id("acme/repo-beta", 7)


def test_refresh_that_drops_a_merged_pr_does_not_push_it(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """A merged PR the refresh omits keeps the opening snapshot.

    The fix clears the fake search. Replacing the map would forget gamma and
    open a new PR.
    """
    fix_dir, origins = _setup(tmp_path)
    _write_fix(
        fix_dir,
        body=(
            "    from gh_harness import FakeGh\n"
            f"    FakeGh(Path({str(fake_gh.root)!r})).configure(search_items=[])\n"
            "    (repo / 'README.md').write_text('changed')\n"
            "    return True"
        ),
    )
    fake_gh.configure(
        search_items=[
            search_item(
                "acme/repo-gamma", 9, "closed", merged_at="2026-09-01T00:00:00Z"
            ),
        ]
    )

    with _mock_sync(repos=[REPO_C]), offline_clone():
        assert run_fix.main([str(fix_dir), "--apply", "--workers", "1"]) == 0

    assert fake_gh.count("api", "--method") == 2
    assert fake_gh.count("pr", "create") == 0
    assert fake_gh.calls_for("acme/repo-gamma") == []
    assert not branch_pushed(origins["acme/repo-gamma"], BRANCH)
    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-gamma"]["status"] == "pr_merged"
    assert (
        status["acme/repo-gamma"]["pr_url"]
        == "https://github.com/acme/repo-gamma/pull/9"
    )
    assert "pushed_sha" not in status["acme/repo-gamma"]


def test_pr_missing_from_stale_search_falls_back_to_the_existing_pr(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """Search lags behind reality: alpha's PR exists but wasn't returned.

    `gh pr create` reports it already exists; we recover with a single view
    instead of failing or opening a duplicate.
    """
    fix_dir, _ = _setup(tmp_path)
    fake_gh.configure(
        already_exists=["acme/repo-alpha"],
        views={
            "acme/repo-alpha": {
                "url": "https://github.com/acme/repo-alpha/pull/3",
                "state": "OPEN",
            }
        },
    )

    with _mock_sync(repos=[REPO_A]), offline_clone():
        run_fix.main([str(fix_dir), "--apply", "--workers", "1"])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-alpha"]["status"] == "pr_created"
    assert (
        status["acme/repo-alpha"]["pr_url"]
        == "https://github.com/acme/repo-alpha/pull/3"
    )


def test_dry_run_makes_no_github_api_calls(tmp_path: Path, fake_gh: FakeGh) -> None:
    fix_dir, _ = _setup(tmp_path)

    with _mock_sync(repos=[REPO_A, REPO_B, REPO_C]), offline_clone():
        run_fix.main([str(fix_dir), "--workers", "1"])

    assert fake_gh.calls() == []


def test_creating_a_pr_costs_exactly_one_api_call_and_records_its_url(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """Per new PR: one `pr create`, whose output is the URL. Nothing else.

    Plus, for the whole run, two searches (before the fixes, and again before
    any push) and one batched lookup that caches the new PRs' node ids.
    """
    fix_dir, _ = _setup(tmp_path)

    with _mock_sync(repos=[REPO_A, REPO_B, REPO_C]), offline_clone():
        run_fix.main([str(fix_dir), "--apply", "--workers", "1"])

    verbs = [args[:2] for args in (c["args"] for c in fake_gh.calls())]
    assert verbs == (
        [["api", "--method"], ["api", "--method"]]
        + [["pr", "create"]] * 3
        + [["api", "graphql"]]
    )

    status = load_status(fix_dir / "status.yaml")
    for repo in ("alpha", "beta", "gamma"):
        assert status[f"acme/repo-{repo}"]["pr_url"] == (
            f"https://github.com/acme/repo-{repo}/pull/1"
        )


def test_create_that_prints_no_url_is_recovered_with_one_view(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    fix_dir, _ = _setup(tmp_path)
    fake_gh.configure(
        create_prints_url=False,
        views={
            "acme/repo-alpha": {
                "url": "https://github.com/acme/repo-alpha/pull/5",
                "state": "OPEN",
            }
        },
    )

    with _mock_sync(repos=[REPO_A]), offline_clone():
        run_fix.main([str(fix_dir), "--apply", "--workers", "1"])

    assert fake_gh.count("pr", "create") == 1
    assert fake_gh.count("pr", "view") == 1
    status = load_status(fix_dir / "status.yaml")
    assert (
        status["acme/repo-alpha"]["pr_url"]
        == "https://github.com/acme/repo-alpha/pull/5"
    )


def test_concurrent_workers_still_space_out_pr_creations(
    tmp_path: Path, fake_gh: FakeGh, monkeypatch: pytest.MonkeyPatch
) -> None:
    """Creating PRs is rate limited harder than reads, even across threads.

    Three workers race to open PRs; the recorded call times must still be at
    least the creation delay apart.
    """
    delay = 0.4
    monkeypatch.setattr(pr_create_limiter, "_min_delay", delay)
    fix_dir, _ = _setup(tmp_path)

    with _mock_sync(repos=[REPO_A, REPO_B, REPO_C]), offline_clone():
        run_fix.main([str(fix_dir), "--apply", "--workers", "3"])

    times = sorted(
        c["time"] for c in fake_gh.calls() if c["args"][:2] == ["pr", "create"]
    )
    assert len(times) == 3
    gaps = [later - earlier for earlier, later in pairwise(times)]
    # Small tolerance: timestamps are taken inside the subprocess after startup.
    assert all(gap >= delay - 0.05 for gap in gaps), gaps


def test_node_ids_are_cached_for_every_pr_with_one_batched_lookup_for_new_ones(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """beta's and gamma's ids arrive free with the search; only alpha, whose PR was
    just created, needs resolving, and that is a single GraphQL call."""
    fix_dir, _ = _setup(tmp_path)
    fake_gh.configure(
        search_items=[
            search_item("acme/repo-beta", 7, "open"),
            search_item(
                "acme/repo-gamma", 9, "closed", merged_at="2026-09-01T00:00:00Z"
            ),
        ]
    )

    with _mock_sync(repos=[REPO_A, REPO_B, REPO_C]), offline_clone():
        run_fix.main([str(fix_dir), "--apply", "--workers", "1"])

    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-alpha"]["pr_node_id"] == node_id("acme/repo-alpha", 1)
    assert status["acme/repo-beta"]["pr_node_id"] == node_id("acme/repo-beta", 7)
    assert status["acme/repo-gamma"]["pr_node_id"] == node_id("acme/repo-gamma", 9)

    verbs = [c["args"][:2] for c in fake_gh.calls()]
    assert verbs == [
        ["api", "--method"],
        ["api", "--method"],
        ["pr", "create"],
        ["api", "graphql"],
    ]


def test_a_second_run_with_everything_cached_makes_no_api_calls(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    fix_dir, _ = _setup(tmp_path)

    with _mock_sync(repos=[REPO_A, REPO_B]), offline_clone():
        run_fix.main([str(fix_dir), "--apply", "--workers", "1"])
        calls_after_first_run = len(fake_gh.calls())
        run_fix.main([str(fix_dir), "--apply", "--workers", "1"])

    assert calls_after_first_run > 0
    assert len(fake_gh.calls()) == calls_after_first_run


def test_node_id_lookup_is_batched_fifty_urls_per_call(fake_gh: FakeGh) -> None:
    urls = [f"https://github.com/acme/service-{n}/pull/{n}" for n in range(120)]

    ids = git.resolve_pr_node_ids(urls)

    assert len(ids) == 120
    assert ids[urls[7]] == node_id("acme/service-7", 7)
    assert ids[urls[119]] == node_id("acme/service-119", 119)
    queries = [c["args"] for c in fake_gh.calls()]
    assert len(queries) == 3  # 50 + 50 + 20
    assert [q[-1].count("resource(url") for q in queries] == [50, 50, 20]
