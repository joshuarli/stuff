from __future__ import annotations

import argparse
import re
from pathlib import Path

import pytest

from repomod import github
from repomod.catalog import load_catalog
from repomod.cli import _discover, _fix, _status
from repomod.fix import FixOutcome
from repomod.policy import filter_repositories, lint_repositories, selected_rules
from repomod.types import CheckResult, RemoteRepository, RepoId, Repository, TagPredicate


def repo(name: str = "tool", tags: tuple[str, ...] = ("tool",)) -> Repository:
    return Repository(RepoId("acme", name), tags)


def test_ci_states_distinguish_empty_incomplete_failure_pending_and_skips() -> None:
    assert github._state_for_checks((), True) == "none"
    assert github._state_for_checks((), False) == "unknown"
    assert github._state_for_checks(
        (CheckResult("build", "failure", None),), True
    ) == "failed"
    assert github._state_for_checks(
        (
            CheckResult("old success", "success", None),
            CheckResult("rerun", "failure", None),
        ),
        True,
    ) == "failed"
    assert github._state_for_checks(
        (CheckResult("build", "in_progress", None),), True
    ) == "pending"
    assert github._state_for_checks(
        (CheckResult("build", "cancelled", None),), True
    ) == "cancelled"
    assert github._state_for_checks(
        (CheckResult("workflow", "skipped", None),), True
    ) == "skipped"
    assert github._state_for_checks(
        (CheckResult("external", "neutral", None),), True
    ) == "neutral"
    assert github._state_for_checks(
        (CheckResult("build", "success", None),), True
    ) == "passed"


def test_latest_check_attempt_controls_the_aggregate_state() -> None:
    assert github._state_for_checks(
        (
            CheckResult(
                "build", "success", None, "GitHub Actions", "2026-10-20T12:00:00Z"
            ),
            CheckResult(
                "build", "failure", None, "GitHub Actions", "2026-10-21T12:00:00Z"
            ),
        ),
        True,
    ) == "failed"
    assert github._state_for_checks(
        (
            CheckResult(
                "build", "failure", None, "GitHub Actions", "2026-10-20T12:00:00Z"
            ),
            CheckResult(
                "build", "success", None, "GitHub Actions", "2026-10-21T12:00:00Z"
            ),
        ),
        True,
    ) == "passed"


def test_ci_check_contexts_are_paginated_and_include_external_statuses(
    monkeypatch,
) -> None:
    pages = iter(
        [
            {
                "data": {
                    "r0": {
                        "object": {
                            "statusCheckRollup": {
                                "contexts": {
                                    "nodes": [
                                        {
                                            "__typename": "CheckRun",
                                            "name": "build",
                                            "status": "COMPLETED",
                                            "conclusion": "SUCCESS",
                                            "detailsUrl": "https://github.invalid/old",
                                            "startedAt": "2026-10-20T12:00:00Z",
                                            "completedAt": "2026-10-20T12:01:00Z",
                                            "app": {"name": "GitHub Actions"},
                                        }
                                    ],
                                    "pageInfo": {
                                        "hasNextPage": True,
                                        "endCursor": "next",
                                    },
                                }
                            }
                        }
                    }
                }
            },
            {
                "data": {
                    "r0": {
                        "object": {
                            "statusCheckRollup": {
                                "contexts": {
                                    "nodes": [
                                        {
                                            "__typename": "StatusContext",
                                            "context": "deploy preview",
                                            "state": "FAILURE",
                                            "targetUrl": "https://github.invalid/new",
                                            "createdAt": "2026-10-21T12:00:00Z",
                                        }
                                    ],
                                    "pageInfo": {
                                        "hasNextPage": False,
                                        "endCursor": "next",
                                    },
                                }
                            }
                        }
                    }
                }
            },
        ]
    )
    queries: list[str] = []

    def graphql(query: str):
        queries.append(query)
        return next(pages)

    monkeypatch.setattr(github, "_graphql", graphql)
    checks, errors = github._status_check_pages(
        {"r0": (RepoId("acme", "tool"), "a" * 40)}
    )

    assert not errors
    assert len(queries) == 2
    assert 'after: "next"' in queries[1]
    assert len(checks["r0"][0]) == 2
    assert github._state_for_checks(*checks["r0"]) == "failed"


def test_ci_check_queries_are_batched_for_a_large_fleet(monkeypatch) -> None:
    queries: list[str] = []

    def graphql(query: str):
        queries.append(query)
        aliases = re.findall(r"(r\d+): repository", query)
        return {
            "data": {
                alias: {
                    "object": {
                        "statusCheckRollup": {
                            "contexts": {
                                "nodes": [],
                                "pageInfo": {"hasNextPage": False},
                            }
                        }
                    }
                }
                for alias in aliases
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)
    pending = {
        f"r{index}": (RepoId("acme", f"tool-{index}"), "a" * 40)
        for index in range(45)
    }

    checks, errors = github._status_check_pages(pending)

    assert not errors
    assert set(checks) == set(pending)
    assert len(queries) == 3
    assert all(len(re.findall(r"r\d+: repository", query)) <= 20 for query in queries)


def test_offline_ci_uses_cache_without_any_network_calls(
    tmp_path: Path, monkeypatch
) -> None:
    cache_path = tmp_path / "ci.json"
    github._write_cache(
        cache_path,
        {
            "acme/tool@abc123": {
                "default_branch": "trunk",
                "sha": "abc123",
                "state": "passed",
                "checks": [
                    {
                        "name": "CI",
                        "state": "success",
                        "url": "https://github.invalid/check",
                    }
                ],
                "fetched_at": 900,
            }
        },
    )
    monkeypatch.setattr(
        github, "remote_metadata", lambda _: pytest.fail("offline mode queried GitHub")
    )
    monkeypatch.setattr(
        github,
        "_status_check_pages",
        lambda _: pytest.fail("offline mode queried check API"),
    )

    result = github.ci_status(
        (repo(),), offline=True, cache_path=cache_path, now=1000
    )["acme/tool"]

    assert result.state == "passed"
    assert result.cached
    assert result.age_seconds == 100
    assert result.sha == "abc123"
    assert result.default_branch == "trunk"
    refreshed = github.ci_status(
        (repo(),),
        offline=True,
        refresh=True,
        cache_path=cache_path,
        now=1000,
    )["acme/tool"]
    assert refreshed.state == "unknown"
    assert not refreshed.cached


def test_ci_cache_is_keyed_to_current_remote_default_head(
    tmp_path: Path, monkeypatch
) -> None:
    repository = repo()
    current_sha = "1" * 40
    next_sha = "2" * 40
    remote = RemoteRepository("acme/tool", "trunk", current_sha, False)
    queried: list[str] = []

    def checks(pending):
        queried.extend(value[1] for value in pending.values())
        return {
            "r0": ((CheckResult("CI", "failure", "https://github.invalid/run"),), True)
        }, {}

    monkeypatch.setattr(github, "_status_check_pages", checks)
    current = github.ci_status(
        (repository,),
        {"acme/tool": remote},
        cache_path=tmp_path / "ci.json",
        now=1000,
    )["acme/tool"]
    assert current.default_branch == "trunk"
    assert current.sha == current_sha
    assert current.state == "failed"
    assert queried == [current_sha]

    new_remote = RemoteRepository("acme/tool", "trunk", next_sha, False)
    monkeypatch.setattr(
        github,
        "_status_check_pages",
        lambda pending: ({"r0": ((), True)}, {}),
    )
    advanced = github.ci_status(
        (repository,),
        {"acme/tool": new_remote},
        cache_path=tmp_path / "ci.json",
        now=1001,
    )["acme/tool"]
    assert advanced.sha == next_sha
    assert advanced.state == "none"
    assert not advanced.cached


def test_stale_cached_ci_is_never_reported_as_passed_when_remote_is_unavailable(
    tmp_path: Path,
) -> None:
    cache_path = tmp_path / "ci.json"
    github._write_cache(
        cache_path,
        {
            "acme/tool@old-sha": {
                "default_branch": "main",
                "sha": "old-sha",
                "state": "passed",
                "checks": [],
                "fetched_at": 1000,
            }
        },
    )

    result = github.ci_status(
        (repo(),),
        {
            "acme/tool": RemoteRepository(
                "acme/tool", None, None, None, "API unavailable"
            )
        },
        cache_path=cache_path,
        now=1001,
    )["acme/tool"]

    assert result.state == "stale"
    assert result.sha == "old-sha"
    assert result.error == "API unavailable"


def test_remote_metadata_uses_the_repository_default_branch_and_exact_sha(
    monkeypatch,
) -> None:
    def graphql(query: str):
        assert "defaultBranchRef" in query
        return {
            "data": {
                "r0": {
                    "isArchived": False,
                    "defaultBranchRef": {"name": "trunk", "target": {"oid": "3" * 40}},
                }
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)

    result = github.remote_metadata((repo(),))["acme/tool"]

    assert result.default_branch == "trunk"
    assert result.sha == "3" * 40
    assert result.archived is False


def test_topics_apply_replaces_the_full_set_and_skips_noop(monkeypatch) -> None:
    repo_value = Repository(RepoId("laputa-systems", "srv"), ("tool",))
    calls: list[tuple[list[str], str | None]] = []

    def gh_json(arguments: list[str], input_text: str | None = None):
        calls.append((arguments, input_text))
        if "--method" in arguments:
            return {"names": ["tool"]}
        return {"names": ["file-browser", "file-server", "http-server"]}

    monkeypatch.setattr(github, "_gh_json", gh_json)

    changed, additions, removals = github.topics_apply(repo_value)

    assert changed
    assert additions == ("tool",)
    assert removals == ("file-browser", "file-server", "http-server")
    assert calls[1][1] == '{"names":["tool"]}'
    assert calls[1][0][1:3] == ["--method", "PUT"]


def test_topics_noop_and_permission_errors_are_reported_without_stopping_others(
    monkeypatch,
) -> None:
    first = repo()
    second = Repository(RepoId("acme", "other"), ("tool",))

    def get_topics(arguments: list[str], input_text: str | None = None):
        if "other" in arguments[0]:
            raise github.GitHubError("HTTP 403 Resource not accessible")
        return {"names": ["tool"]}

    monkeypatch.setattr(github, "_gh_json", get_topics)

    results = github.topics_many((first, second), apply=True)

    assert results[0][0] == "acme/other"
    assert "403" in (results[0][4] or "")
    assert results[1][0] == "acme/tool"
    assert results[1][1] is False


def test_rule_selection_is_independent_of_repository_tag_filters() -> None:
    rule_ids = {rule.id for rule in selected_rules()}

    assert {"gha-pin", "repo-integrity", "topics", "repo-template"} <= rule_ids
    assert {"cargo-release", "python-release", "npm-release"} <= rule_ids


def test_conjunctive_applicability_separates_role_language_and_publishing_tags() -> None:
    rust_library = TagPredicate(all_of=frozenset({"library", "rust"}))

    assert not rust_library.matches(("library",))
    assert not rust_library.matches(("rust",))
    assert not rust_library.matches(("crates-io",))
    assert rust_library.matches(("library", "rust"))
    assert rust_library.matches(("tool", "library", "rust"))
    assert not rust_library.matches(("library", "typescript"))


def test_language_filter_selects_repositories_independently_of_roles() -> None:
    app = repo("app", ("app", "rust", "typescript"))
    library_tool = repo("shadowdriver", ("library", "tool", "rust", "typescript"))
    tool = repo("srv", ("tool",))
    candidates = (app, library_tool, tool)

    assert filter_repositories(candidates, tags=("typescript",)) == (app, library_tool)
    assert filter_repositories(candidates, tags=("tool",)) == (library_tool, tool)


def test_autofix_override_does_not_remove_repository_from_lint(tmp_path: Path) -> None:
    path = tmp_path / "catalog.toml"
    path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n\n'
        '[overrides."acme/demo"]\nautofix = false\nreason = "upstream template"\n',
        encoding="utf-8",
    )
    catalog = load_catalog(
        path,
        {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path / "repos")},
    )
    findings = lint_repositories(catalog, local_only=True)

    assert not catalog.repositories[0].autofix
    assert any(
        finding.repository == "acme/demo" and finding.rule == "repo-integrity"
        for finding in findings
    )


def test_status_offline_never_calls_remote_metadata(
    tmp_path: Path, monkeypatch
) -> None:
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    monkeypatch.setattr(
        "repomod.cli._catalog",
        lambda: load_catalog(catalog_path, {"HOME": str(tmp_path)}),
    )
    monkeypatch.setattr(
        "repomod.cli.remote_metadata",
        lambda _: pytest.fail("offline status called GitHub"),
    )
    args = argparse.Namespace(
        repo=[],
        tag=[],
        offline=True,
        refresh=False,
        dirty=False,
        failed=False,
        json=True,
        details=False,
    )

    assert _status(args) == 0


def test_discover_queries_both_configured_fleet_owners(monkeypatch, capsys) -> None:
    catalog_path = Path(__file__).resolve().parents[1] / "catalog.toml"
    catalog = load_catalog(catalog_path)
    owners: list[str] = []
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    monkeypatch.setattr(
        "repomod.cli.discover", lambda values: owners.extend(values) or []
    )

    assert _discover(argparse.Namespace()) == 0
    assert owners == ["joshuarli", "laputa-systems"]
    assert "no uncatalogued" in capsys.readouterr().out


def test_fix_reports_cleanup_failure_as_nonzero(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(catalog_path, {"HOME": str(tmp_path)})
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    monkeypatch.setattr(
        "repomod.cli.remote_metadata",
        lambda _: {
            "acme/demo": RemoteRepository(
                "acme/demo", "main", "a" * 40, False
            )
        },
    )
    monkeypatch.setattr(
        "repomod.cli.fix_repository",
        lambda *_: FixOutcome("acme/demo", cleanup_error="worktree remains"),
    )
    args = argparse.Namespace(
        repo=[], tag=[], rule=[], workers=1, push=False
    )

    assert _fix(args) == 1
    assert "worktree remains" in capsys.readouterr().err
