"""--undraft against a fake gh: which calls it makes and which PRs it touches."""

from pathlib import Path
from typing import Any

import pytest
from gh_harness import FakeGh, node_id, search_item
from test_main import _write_fix

from repomod import main as run_fix
from repomod.status import atomic_write_yaml, load_status

LOOKUP = "nodes(ids"
MUTATION = "markPullRequestReadyForReview"
RESOLVE = "resource(url"


def _entry(repo: str, number: int, *, cached: bool = True) -> dict[str, str]:
    entry = {
        "status": "pr_created",
        "pr_url": f"https://github.com/{repo}/pull/{number}",
        "fix_hash": "abc123",
    }
    if cached:
        entry["pr_node_id"] = node_id(repo, number)
    return entry


def _setup(tmp_path: Path, entries: dict[str, dict[str, str]]) -> Path:
    fix_dir = tmp_path / "fix"
    _write_fix(fix_dir)
    atomic_write_yaml(entries, fix_dir / "status.yaml")
    return fix_dir


def _mixed_rollout(tmp_path: Path, fake_gh: FakeGh) -> Path:
    """alpha, beta: drafts with cached ids. gamma: draft whose id was never cached.
    delta: already marked ready by hand. epsilon: merged since."""
    fix_dir = _setup(
        tmp_path,
        {
            "acme/repo-alpha": _entry("acme/repo-alpha", 1),
            "acme/repo-beta": _entry("acme/repo-beta", 2),
            "acme/repo-gamma": _entry("acme/repo-gamma", 3, cached=False),
            "acme/repo-delta": _entry("acme/repo-delta", 4),
            "acme/repo-epsilon": _entry("acme/repo-epsilon", 5),
            # Not a created PR, so never a candidate.
            "acme/repo-zeta": {"status": "pending", "fix_hash": "abc123"},
        },
    )
    fake_gh.configure(
        search_items=[
            search_item("acme/repo-alpha", 1, "open", draft=True),
            search_item("acme/repo-beta", 2, "open", draft=True),
            search_item("acme/repo-gamma", 3, "open", draft=True),
            search_item("acme/repo-delta", 4, "open", draft=False),
            search_item("acme/repo-epsilon", 5, "closed", "2026-10-01T00:00:00Z", True),
        ]
    )
    return fix_dir


def test_dry_run_lists_the_draft_prs_and_changes_nothing(
    tmp_path: Path, fake_gh: FakeGh, capsys: pytest.CaptureFixture[str]
) -> None:
    fix_dir = _mixed_rollout(tmp_path, fake_gh)

    assert run_fix.main([str(fix_dir), "--undraft"]) == 0

    out = capsys.readouterr().out
    for repo in ("repo-alpha", "repo-beta", "repo-gamma"):
        assert f"acme/{repo}" in out
    for repo in ("repo-delta", "repo-epsilon", "repo-zeta"):
        assert f"acme/{repo}" not in out
    assert "pass --apply" in out
    assert fake_gh.graphql(MUTATION) == []
    # gamma's missing id is resolved once and cached for next time.
    assert len(fake_gh.graphql(RESOLVE)) == 1
    status = load_status(fix_dir / "status.yaml")
    assert status["acme/repo-gamma"]["pr_node_id"] == node_id("acme/repo-gamma", 3)


def test_apply_marks_only_draft_open_prs_ready_in_one_mutation(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    fix_dir = _mixed_rollout(tmp_path, fake_gh)

    assert run_fix.main([str(fix_dir), "--undraft", "--apply"]) == 0

    (mutation,) = fake_gh.graphql(MUTATION)
    for repo, number in (("alpha", 1), ("beta", 2), ("gamma", 3)):
        assert node_id(f"acme/repo-{repo}", number) in mutation
    for repo, number in (("delta", 4), ("epsilon", 5)):
        assert node_id(f"acme/repo-{repo}", number) not in mutation
    # One state lookup for all five cached ids, one resolve for gamma, one mutation.
    assert len(fake_gh.graphql(LOOKUP)) == 1
    assert len(fake_gh.graphql(RESOLVE)) == 1
    assert len(fake_gh.calls()) == 3


def test_a_second_undraft_finds_nothing_left_and_does_not_mutate(
    tmp_path: Path, fake_gh: FakeGh, capsys: pytest.CaptureFixture[str]
) -> None:
    fix_dir = _mixed_rollout(tmp_path, fake_gh)
    run_fix.main([str(fix_dir), "--undraft", "--apply"])
    capsys.readouterr()

    assert run_fix.main([str(fix_dir), "--undraft", "--apply"]) == 0

    assert len(fake_gh.graphql(MUTATION)) == 1  # still just the first run's
    assert "No draft PRs" in capsys.readouterr().out


def test_large_rollouts_are_looked_up_and_marked_in_batches(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    """45 PRs: one state lookup (up to 100 ids per call) and mutations of 20/20/5."""
    repos = [f"acme/service-{n}" for n in range(45)]
    fix_dir = _setup(tmp_path, {r: _entry(r, n + 1) for n, r in enumerate(repos)})
    fake_gh.configure(
        search_items=[
            search_item(r, n + 1, "open", draft=True) for n, r in enumerate(repos)
        ]
    )

    assert run_fix.main([str(fix_dir), "--undraft", "--apply"]) == 0

    assert len(fake_gh.graphql(LOOKUP)) == 1
    mutations = fake_gh.graphql(MUTATION)
    assert [m.count(MUTATION) for m in mutations] == [20, 20, 5]
    assert len(fake_gh.calls()) == 4


def test_repos_and_limit_narrow_what_is_promoted(
    tmp_path: Path, fake_gh: FakeGh
) -> None:
    fix_dir = _mixed_rollout(tmp_path, fake_gh)

    run_fix.main(
        [
            str(fix_dir),
            "--undraft",
            "--apply",
            "--repos",
            "acme/repo-alpha,acme/repo-beta",
        ]
    )
    (mutation,) = fake_gh.graphql(MUTATION)
    assert node_id("acme/repo-alpha", 1) in mutation
    assert node_id("acme/repo-beta", 2) in mutation
    assert node_id("acme/repo-gamma", 3) not in mutation


def test_limit_caps_how_many_are_promoted(tmp_path: Path, fake_gh: FakeGh) -> None:
    fix_dir = _mixed_rollout(tmp_path, fake_gh)

    run_fix.main([str(fix_dir), "--undraft", "--apply", "--limit", "2"])

    (mutation,) = fake_gh.graphql(MUTATION)
    assert mutation.count(MUTATION) == 2


def test_refused_prs_are_reported_and_the_rest_still_go_ready(
    tmp_path: Path, fake_gh: FakeGh, capsys: pytest.CaptureFixture[str]
) -> None:
    fix_dir = _mixed_rollout(tmp_path, fake_gh)
    configured: dict[str, Any] = {
        "graphql_fail": [node_id("acme/repo-beta", 2)],
    }
    fake_gh.configure(
        search_items=[
            search_item("acme/repo-alpha", 1, "open", draft=True),
            search_item("acme/repo-beta", 2, "open", draft=True),
            search_item("acme/repo-gamma", 3, "open", draft=True),
        ],
        **configured,
    )

    assert run_fix.main([str(fix_dir), "--undraft", "--apply"]) == 1

    out = capsys.readouterr().out
    assert "1 PRs could not be marked ready" in out
    assert "Marked ready: 2 PRs" in out
    assert "✗" in out
    assert "acme/repo-beta" in out


def test_nothing_recorded_means_nothing_to_do(
    tmp_path: Path, fake_gh: FakeGh, capsys: pytest.CaptureFixture[str]
) -> None:
    fix_dir = _setup(tmp_path, {})

    assert run_fix.main([str(fix_dir), "--undraft", "--apply"]) == 0

    assert "No draft PRs" in capsys.readouterr().out
    assert fake_gh.calls() == []
