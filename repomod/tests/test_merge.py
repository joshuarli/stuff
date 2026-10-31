from __future__ import annotations

import argparse

import pytest

from repomod import cli as cli_module
from repomod import github
from repomod.catalog import load_catalog
from repomod.cmd.merge import _merge
from repomod.types import RepoId, RepomodPullRequest, Repository


def repo(name: str = "demo") -> Repository:
    return Repository(RepoId("acme", name), ("tool",))


def catalog(tmp_path, monkeypatch) -> None:
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\nother = ["tool"]\n',
        encoding="utf-8",
    )
    loaded = load_catalog(catalog_path, {"HOME": str(tmp_path)})
    monkeypatch.setattr("repomod.cmd.common._catalog", lambda: loaded)


def use_listing(monkeypatch: pytest.MonkeyPatch, requests) -> None:
    def listing(repositories, jobs, *, on_complete=None):
        assert jobs >= 1
        if on_complete is not None:
            on_complete(len(repositories), len(repositories))
        return requests

    monkeypatch.setattr(github, "repomod_pull_requests", listing)


def pull_request(number: int, branch: str, sha: str, *, draft: bool = False) -> RepomodPullRequest:
    return RepomodPullRequest(
        number,
        f"https://github.invalid/acme/demo/pull/{number}",
        branch,
        sha,
        draft,
    )


def test_repomod_pull_requests_selects_fix_branches_from_the_same_repository(monkeypatch) -> None:
    branch = "repomod/fix/" + "a" * 32
    draft_branch = "repomod/fix/" + "b" * 32
    sha = "c" * 40
    calls: list[list[str]] = []

    def gh_json(arguments: list[str]) -> object:
        calls.append(arguments)
        return [
            [
                {
                    "number": 7,
                    "html_url": "https://github.invalid/acme/demo/pull/7",
                    "title": "chore(repomod): apply automatic fixes",
                    "draft": False,
                    "head": {
                        "ref": branch,
                        "sha": sha.upper(),
                        "repo": {"full_name": "acme/demo"},
                    },
                },
                {
                    "number": 8,
                    "html_url": "https://github.invalid/acme/demo/pull/8",
                    "title": "manual branch",
                    "draft": False,
                    "head": {"ref": "feature/manual", "sha": sha, "repo": {"full_name": "acme/demo"}},
                },
                {
                    "number": 9,
                    "html_url": "https://github.invalid/acme/demo/pull/9",
                    "title": "fork branch",
                    "draft": False,
                    "head": {"ref": draft_branch, "sha": sha, "repo": {"full_name": "acme/fork"}},
                },
                {
                    "number": 10,
                    "html_url": "https://github.invalid/acme/demo/pull/10",
                    "title": "draft work",
                    "draft": True,
                    "head": {"ref": draft_branch, "sha": sha, "repo": {"full_name": "acme/demo"}},
                },
            ]
        ]

    monkeypatch.setattr(github, "_gh_json", gh_json)

    requests, error = github.repomod_pull_requests((repo(),))["acme/demo"]

    assert error is None
    assert [(request.number, request.branch, request.head_sha, request.draft) for request in requests] == [
        (7, branch, sha, False),
        (10, draft_branch, sha, True),
    ]
    assert calls == [
        ["--paginate", "--slurp", "repos/acme/demo/pulls?state=open&per_page=100"]
    ]


def test_repomod_pull_requests_reports_listing_progress(monkeypatch) -> None:
    monkeypatch.setattr(github, "_gh_json", lambda _: [[]])
    progress: list[tuple[int, int]] = []

    github.repomod_pull_requests(
        (repo("demo"), repo("other")),
        jobs=1,
        on_complete=lambda completed, total: progress.append((completed, total)),
    )

    assert sorted(progress) == [(1, 2), (2, 2)]


def test_repomod_pull_requests_reports_listing_errors(monkeypatch) -> None:
    monkeypatch.setattr(github, "_gh_json", lambda _: {"not": "a list"})
    assert github.repomod_pull_requests((repo(),)) == {
        "acme/demo": ([], "GitHub returned an invalid pull request list")
    }

    monkeypatch.setattr(github, "_gh_json", lambda _: [{"not": "a page"}])
    assert github.repomod_pull_requests((repo(),)) == {
        "acme/demo": ([], "GitHub returned an invalid pull request page")
    }

    def fail(_arguments: list[str]) -> object:
        raise github.GitHubError("boom")

    monkeypatch.setattr(github, "_gh_json", fail)
    assert github.repomod_pull_requests((repo(),)) == {"acme/demo": ([], "boom")}
    assert github.repomod_pull_requests(()) == {}


def test_merge_repomod_pull_request_pins_the_listed_head(monkeypatch) -> None:
    head = "c" * 40
    merged = "d" * 40
    calls: list[list[str]] = []

    def gh_json(arguments: list[str]) -> object:
        calls.append(arguments)
        return {"merged": True, "sha": merged}

    monkeypatch.setattr(github, "_gh_json", gh_json)

    assert github.merge_repomod_pull_request(RepoId("acme", "demo"), 7, head) == merged
    assert calls == [
        [
            "repos/acme/demo/pulls/7/merge",
            "--method",
            "PUT",
            "-f",
            "merge_method=rebase",
            "-f",
            f"sha={head}",
        ]
    ]

    monkeypatch.setattr(github, "_gh_json", lambda _: {"merged": False})
    with pytest.raises(github.GitHubError, match="did not confirm the merge"):
        github.merge_repomod_pull_request(RepoId("acme", "demo"), 7, head)

    with pytest.raises(github.GitHubError, match="number is invalid"):
        github.merge_repomod_pull_request(RepoId("acme", "demo"), 0, head)
    with pytest.raises(github.GitHubError, match="head SHA is invalid"):
        github.merge_repomod_pull_request(RepoId("acme", "demo"), 7, "not-a-sha")


def test_merge_command_merges_fix_branches_and_reports_failures(
    tmp_path, monkeypatch, capsys
) -> None:
    catalog(tmp_path, monkeypatch)
    branch = "repomod/fix/" + "a" * 32
    draft_branch = "repomod/fix/" + "b" * 32
    requests = {
        "acme/demo": ([pull_request(3, branch, "c" * 40), pull_request(4, draft_branch, "e" * 40, draft=True)], None),
        "acme/other": ([pull_request(5, branch, "f" * 40)], None),
    }
    merged: list[tuple[str, int, str]] = []

    def merge_pull_request(repository: RepoId, number: int, head_sha: str) -> str:
        merged.append((str(repository), number, head_sha))
        if number == 5:
            raise github.GitHubError("pull request is not mergeable")
        return "d" * 40

    use_listing(monkeypatch, requests)
    monkeypatch.setattr(github, "merge_repomod_pull_request", merge_pull_request)
    args = argparse.Namespace(repo=[], tag=[], dry_run=False, workers=2)

    assert _merge(args) == 1
    output = capsys.readouterr()
    assert sorted(merged) == [
        ("acme/demo", 3, "c" * 40),
        ("acme/other", 5, "f" * 40),
    ]
    assert "acme/demo: #3 merged dddddddddddd" in output.out
    assert "acme/demo: #4 skipped: pull request is a draft" in output.err
    assert "acme/other: #5 failed: pull request is not mergeable" in output.err
    assert "Listing repomod pull requests: 2/2 complete" in output.err
    assert "Found 3 repomod pull requests across 2 repositories." in output.err
    assert (
        "summary: 3 repomod pull requests in 2 repositories; merged 1; skipped 1; failed 1"
        in output.out
    )


def test_merge_command_dry_run_does_not_merge(tmp_path, monkeypatch, capsys) -> None:
    catalog(tmp_path, monkeypatch)
    branch = "repomod/fix/" + "a" * 32
    requests = {
        "acme/demo": ([pull_request(3, branch, "c" * 40)], None),
        "acme/other": ([], None),
    }
    use_listing(monkeypatch, requests)

    def fail(*_arguments) -> str:
        raise AssertionError("dry run must not merge")

    monkeypatch.setattr(github, "merge_repomod_pull_request", fail)
    args = argparse.Namespace(repo=[], tag=[], dry_run=True, workers=1)

    assert _merge(args) == 0
    output = capsys.readouterr()
    assert f"acme/demo: #3 would merge {branch}" in output.out
    assert "Listing repomod pull requests: 2/2 complete" in output.err
    assert "Found 1 repomod pull request across 2 repositories." in output.err
    assert (
        "summary: 1 repomod pull request in 1 repository; would merge 1; skipped 0; failed 0"
        in output.out
    )


def test_merge_command_reports_when_nothing_is_found(tmp_path, monkeypatch, capsys) -> None:
    catalog(tmp_path, monkeypatch)
    use_listing(monkeypatch, {"acme/demo": ([], None), "acme/other": ([], None)})
    args = argparse.Namespace(repo=[], tag=[], dry_run=False, workers=1)

    assert _merge(args) == 0
    output = capsys.readouterr()
    assert "Found 0 repomod pull requests across 2 repositories." in output.err
    assert "summary: no repomod pull requests in 2 repositories" in output.out


def test_merge_command_fails_when_a_repository_listing_fails(tmp_path, monkeypatch, capsys) -> None:
    catalog(tmp_path, monkeypatch)
    requests = {"acme/demo": ([], "boom"), "acme/other": ([], None)}
    use_listing(monkeypatch, requests)
    args = argparse.Namespace(repo=[], tag=[], dry_run=False, workers=1)

    assert _merge(args) == 1
    output = capsys.readouterr()
    assert "acme/demo: failed: boom" in output.err
    assert (
        "summary: no repomod pull requests in 1 repository; 1 listing failed" in output.out
    )


def test_merge_is_registered_in_cli_parser() -> None:
    args = cli_module._parser().parse_args(
        ["merge", "--repo", "acme/demo", "--dry-run", "--workers", "2"]
    )

    assert args.command == "merge"
    assert args.repo == ["acme/demo"]
    assert args.tag == []
    assert args.dry_run is True
    assert args.workers == 2
