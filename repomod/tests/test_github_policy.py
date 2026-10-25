from __future__ import annotations

import argparse
import json
import re
import subprocess
from pathlib import Path
from threading import Barrier, Lock

import pytest

from repomod import cli, github
from repomod.catalog import load_catalog
from repomod.cli import _discover, _fix, _lint, _status, _status_text
from repomod.fix import FixOutcome
from repomod.policy import (
    filter_repositories,
    format_finding_short,
    lint_legend,
    lint_repositories,
    selected_rules,
)
from repomod.rules.workflow_settings import (
    _fix_triggers,
    _fix_workflow_settings,
    _trigger_findings,
    _workflow_settings,
)
from repomod.types import (
    Catalog,
    CheckResult,
    CiResult,
    Finding,
    GitStatus,
    OpenPullRequest,
    RemoteRepository,
    RepoId,
    Repository,
    TagPredicate,
)


def repo(name: str = "tool", tags: tuple[str, ...] = ("tool",)) -> Repository:
    return Repository(RepoId("acme", name), tags)


def _check(
    name: str,
    state: str,
    url: str | None = None,
    source: str | None = None,
    updated_at: str | None = None,
    check_suite_id: str | None = None,
    workflow_run_id: str | None = None,
    run_attempt: int | None = None,
) -> CheckResult:
    return CheckResult(
        name,
        state,
        url,
        source,
        updated_at,
        check_suite_id,
        workflow_run_id,
        run_attempt,
    )


@pytest.mark.parametrize(
    ("checks", "complete", "expected"),
    [
        ((), True, "none"),
        ((), False, "unknown"),
        ((_check("build", "failure"),), True, "failed"),
        (
            (
                _check("old success", "success"),
                _check("rerun", "failure"),
            ),
            True,
            "failed",
        ),
        ((_check("build", "in_progress"),), True, "pending"),
        ((_check("build", "cancelled"),), True, "cancelled"),
        ((_check("workflow", "skipped"),), True, "skipped"),
        ((_check("external", "neutral"),), True, "neutral"),
        ((_check("build", "success"),), True, "passed"),
        (
            (
                _check(
                    "build", "failure", None, "GitHub Actions", "2026-10-20T12:00:00Z",
                    "suite-a", "run-a", 1,
                ),
                _check(
                    "build", "success", None, "GitHub Actions", "2026-10-21T12:00:00Z",
                    "suite-b", "run-a", 2,
                ),
            ),
            True,
            "passed",
        ),
        (
            (
                _check(
                    "build", "failure", None, "GitHub Actions", "2026-10-20T12:00:00Z",
                    "suite-a", None, 1,
                ),
                _check(
                    "build", "cancelled", None, "GitHub Actions", "2026-10-21T12:00:00Z",
                    "suite-a", None, 2,
                ),
            ),
            True,
            "cancelled",
        ),
        (
            (
                _check(
                    "build", "failure", None, "GitHub Actions", "2026-10-21T12:00:00Z",
                    "suite-a", None, 1,
                ),
                _check(
                    "build", "success", None, "GitHub Actions", "2026-10-21T12:01:00Z",
                    "suite-b", None, 1,
                ),
            ),
            True,
            "failed",
        ),
        (
            (
                _check("build", "failure", None, "GitHub Actions", None, "suite-a", None, 1),
                _check("build", "in_progress", None, "GitHub Actions", None, "suite-a", None, 2),
                _check("deploy preview", "success", None, "status:preview", "2026-10-21T12:00:00Z"),
                _check("security scan", "failure", None, "status:scanner", "2026-10-21T12:00:00Z"),
            ),
            True,
            "failed",
        ),
        (
            (
                _check("build", "failure", None, "GitHub Actions", None, "suite-a", None, 1),
                _check("build", "in_progress", None, "GitHub Actions", None, "suite-a", None, 2),
            ),
            True,
            "pending",
        ),
        (
            (
                _check("deploy", "failure", None, "status:preview-a", "2026-10-21T12:00:00Z"),
                _check("deploy", "success", None, "status:preview-b", "2026-10-21T12:01:00Z"),
            ),
            True,
            "failed",
        ),
    ],
)
def test_ci_check_states_aggregate_across_attempts_suites_and_creators(
    checks: tuple[CheckResult, ...], complete: bool, expected: str
) -> None:
    assert github._state_for_checks(checks, complete) == expected


@pytest.mark.parametrize(
    "scenario",
    ["required-checks", "no-checks", "rulesets", "disabled"],
)
def test_fix_pull_request_auto_merge_decisions(monkeypatch, scenario: str) -> None:
    def gh_json(arguments: list[str], input_text: str | None = None):
        endpoint = arguments[0]
        if endpoint == "repos/acme/demo":
            return {"allow_auto_merge": scenario != "disabled"}
        if scenario == "disabled":
            raise AssertionError(f"auto-merge must short-circuit before {endpoint}")
        if endpoint == "repos/acme/demo/branches/main/protection":
            if scenario == "no-checks":
                return {"required_status_checks": None}
            if scenario == "rulesets":
                raise github.GitHubError("HTTP 404 Branch not protected")
            return {"required_status_checks": {"contexts": ["CI"], "checks": []}}
        if endpoint == "repos/acme/demo/rules/branches/main":
            if scenario == "rulesets":
                return [
                    {
                        "type": "required_status_checks",
                        "parameters": {"required_status_checks": [{"context": "CI"}]},
                    }
                ]
            return []
        raise AssertionError(endpoint)

    queries: list[str] = []

    def graphql(query: str):
        queries.append(query)
        return {
            "data": {
                "enablePullRequestAutoMerge": {
                    "pullRequest": {
                        "url": "https://github.invalid/acme/demo/pull/1",
                        "autoMergeRequest": {"enabledAt": "2026-10-23T00:00:00Z"},
                    }
                }
            }
        }

    monkeypatch.setattr(github, "_gh_json", gh_json)
    if scenario in {"no-checks", "disabled"}:
        monkeypatch.setattr(
            github,
            "_graphql",
            lambda _query: (_ for _ in ()).throw(AssertionError("must not enable auto-merge")),
        )
    else:
        monkeypatch.setattr(github, "_graphql", graphql)

    result = github._enable_fix_pull_request_auto_merge(
        RepoId("acme", "demo"), "pull-request-node-id", "main", already_enabled=False
    )

    if scenario == "no-checks":
        assert result == github.AutoMergeSkipped("the default branch has no required status checks")
    elif scenario == "disabled":
        assert result == github.AutoMergeSkipped("repository setting is disabled")
    else:
        assert isinstance(result, github.AutoMergeEnabled)
        assert len(queries) == 1
        assert 'pullRequestId: "pull-request-node-id"' in queries[0]
        assert "mergeMethod: REBASE" in queries[0]


def test_ensure_fix_pull_request_reuses_an_existing_auto_merge_request(monkeypatch) -> None:
    monkeypatch.setattr(
        github,
        "_gh_json",
        lambda _arguments: [
            {
                "html_url": "https://github.invalid/acme/demo/pull/1",
                "node_id": "pull-request-node-id",
                "auto_merge": {"merge_method": "rebase"},
            }
        ],
    )

    result = github.ensure_fix_pull_request(
        RepoId("acme", "demo"), "repomod/fix/" + "a" * 32, "main"
    )

    assert result == github.FixPullRequest(
        "https://github.invalid/acme/demo/pull/1", github.AutoMergeEnabled()
    )


def test_fix_branch_was_merged_checks_the_matching_closed_pull_request(monkeypatch) -> None:
    branch = "repomod/fix/" + "a" * 32
    calls = []

    def gh_json(arguments: list[str], input_text: str | None = None):
        calls.append(arguments)
        return [
            {
                "head": {"ref": branch},
                "merged_at": "2026-10-23T17:00:00Z",
            }
        ]

    monkeypatch.setattr(github, "_gh_json", gh_json)

    assert github.fix_branch_was_merged(RepoId("acme", "demo"), branch)
    assert calls == [
        [
            "repos/acme/demo/pulls",
            "--method",
            "GET",
            "-F",
            "state=closed",
            "-F",
            f"head=acme:{branch}",
            "-F",
            "per_page=100",
        ]
    ]


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
                                            "checkSuite": {
                                                "id": "suite-a",
                                                "workflowRun": {
                                                    "runAttempt": 2,
                                                },
                                            },
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
                                                "updatedAt": "2026-10-21T12:00:01Z",
                                                "creator": {"login": "deploy-app"},
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
    assert "workflowRun { id runAttempt workflow { name } }" in queries[0]
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


def test_partial_graphql_check_errors_are_unknown_and_not_cached(
    tmp_path: Path, monkeypatch
) -> None:
    repositories = (repo("tool"), repo("other"))
    remotes = {
        "acme/tool": RemoteRepository("acme/tool", "main", "1" * 40, False),
        "acme/other": RemoteRepository("acme/other", "main", "2" * 40, False),
    }

    def graphql(_query: str):
        contexts = {"nodes": [], "pageInfo": {"hasNextPage": False}}
        return {
            "data": {
                "r0": {"object": {"statusCheckRollup": {"contexts": contexts}}},
                "r1": {"object": {"statusCheckRollup": {"contexts": contexts}}},
            },
            "errors": [
                {
                    "message": "query resource limit exceeded",
                    "path": ["r0", "repository", "object", "statusCheckRollup"],
                }
            ],
        }

    monkeypatch.setattr(github, "_graphql", graphql)

    results = github.ci_status(
        repositories, remotes, cache_path=tmp_path / "ci.json", now=1000
    )

    assert results["acme/tool"].state == "unknown"
    assert "query resource limit exceeded" in (results["acme/tool"].error or "")
    assert results["acme/other"].state == "none"
    cache, error = github._read_cache(tmp_path / "ci.json")
    assert error is None
    assert set(cache) == {"acme/other@" + "2" * 40}


def test_ci_check_batches_run_concurrently(monkeypatch) -> None:
    barrier = Barrier(4, timeout=2)
    lock = Lock()
    active = 0
    maximum_active = 0
    calls = 0

    def graphql(query: str):
        nonlocal active, maximum_active, calls
        with lock:
            calls += 1
            call_number = calls
            active += 1
            maximum_active = max(maximum_active, active)
        try:
            if call_number <= 4:
                barrier.wait()
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
        finally:
            with lock:
                active -= 1

    monkeypatch.setattr(github, "_graphql", graphql)
    pending = {
        f"r{index}": (RepoId("acme", f"tool-{index}"), "a" * 40)
        for index in range(125)
    }

    checks, errors = github._status_check_pages(pending)

    assert not errors
    assert set(checks) == set(pending)
    assert calls == 7
    assert maximum_active == 4


def test_action_tag_queries_batch_and_follow_each_cursor(monkeypatch) -> None:
    queries: list[str] = []

    def graphql(query: str):
        queries.append(query)
        aliases = re.findall(r"(a\d+): repository", query)
        page = len(queries)
        return {
            "data": {
                alias: {
                    "refs": {
                        "nodes": [{"name": f"v4.{page}.0"}],
                        "pageInfo": {
                            "hasNextPage": page == 1,
                            "endCursor": f"cursor-{alias}" if page == 1 else None,
                        },
                    }
                }
                for alias in aliases
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)
    actions = ("actions/checkout", "actions/setup-python")

    tags = github.action_tags_many(actions, "v")

    assert tags == {
        "actions/checkout": ("v4.1.0", "v4.2.0"),
        "actions/setup-python": ("v4.1.0", "v4.2.0"),
    }
    assert len(queries) == 2
    assert all("refs(first: 100" in query for query in queries)
    assert 'after: "cursor-a0"' in queries[1]
    assert 'after: "cursor-a1"' in queries[1]


def test_resolve_action_tags_batches_and_peels_annotated_tags(monkeypatch) -> None:
    queries: list[str] = []
    commit_sha = "c" * 40

    def graphql(query: str):
        queries.append(query)
        return {
            "data": {
                "a0": {
                    "ref": {
                        "target": {
                            "__typename": "Tag",
                            "oid": "1" * 40,
                            "target": {
                                "__typename": "Tag",
                                "oid": "2" * 40,
                                "target": {"__typename": "Commit", "oid": commit_sha},
                            },
                        }
                    }
                },
                "a1": {
                    "ref": {"target": {"__typename": "Commit", "oid": "d" * 40}}
                },
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)

    resolved = github.resolve_tags_many(
        {"actions/checkout": "v4.5.0", "actions/setup-python": "v6.3.0"}
    )

    assert resolved == {"actions/checkout": commit_sha, "actions/setup-python": "d" * 40}
    assert len(queries) == 1
    assert 'ref(qualifiedName: "refs/tags/v4.5.0")' in queries[0]
    assert '... on Tag { target { __typename oid' in queries[0]


def test_action_tag_batch_reports_partial_graphql_errors(monkeypatch) -> None:
    def graphql(_query: str):
        return {
            "data": {"a0": {"refs": {"nodes": [], "pageInfo": {"hasNextPage": False}}}},
            "errors": [
                {
                    "message": "repository access denied",
                    "path": ["a1", "repository", "refs"],
                }
            ],
        }

    monkeypatch.setattr(github, "_graphql", graphql)

    with pytest.raises(github.GitHubError, match=r"actions/setup-python.*access denied"):
        github.action_tags_many(("actions/checkout", "actions/setup-python"), "v")


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


def test_remote_metadata_batches_run_concurrently(monkeypatch) -> None:
    barrier = Barrier(3, timeout=2)

    def graphql(query: str):
        barrier.wait()
        aliases = re.findall(r"(r\d+): repository", query)
        return {
            "data": {
                alias: {
                    "isArchived": False,
                    "defaultBranchRef": {
                        "name": "main",
                        "target": {"oid": "a" * 40},
                    },
                }
                for alias in aliases
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)
    repositories = tuple(
        Repository(RepoId("acme", f"tool-{index}"), ()) for index in range(45)
    )
    progress: list[tuple[int, int]] = []

    result = github.remote_metadata(
        repositories,
        on_batch_complete=lambda completed, total: progress.append((completed, total)),
    )

    assert len(result) == 45
    assert sorted(progress) == [(1, 3), (2, 3), (3, 3)]


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


def test_status_colors_respect_terminal_and_environment(monkeypatch) -> None:
    monkeypatch.setattr(cli.sys, "stdout", argparse.Namespace(isatty=lambda: True))
    monkeypatch.setenv("TERM", "tmux-256color")

    monkeypatch.setenv("NO_COLOR", "1")
    monkeypatch.delenv("CLICOLOR", raising=False)
    assert cli._paint("FAIL", "31") == "FAIL"

    monkeypatch.delenv("NO_COLOR", raising=False)
    monkeypatch.setenv("CLICOLOR", "1")
    assert cli._paint("FAIL", "31") == "\033[31mFAIL\033[0m"
    assert cli._state_color("failed") == "31"
    assert cli._state_color("passed") == "32"
    assert cli._local_color("dirty (1)") == "33"


def test_discover_queries_both_configured_fleet_owners(tmp_path: Path, monkeypatch, capsys) -> None:
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "person"\n\n'
        '[repos."person"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(catalog_path, {"HOME": str(tmp_path)})
    owners: list[str] = []
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    monkeypatch.setattr(
        "repomod.cli.discover", lambda values: owners.extend(values) or []
    )

    assert _discover(argparse.Namespace()) == 0
    assert owners == ["person", "laputa-systems"]
    assert "no uncatalogued" in capsys.readouterr().out


def test_discover_fetches_owner_lists_concurrently(monkeypatch) -> None:
    barrier = Barrier(2, timeout=2)

    def gh_json(arguments: list[str], input_text: str | None = None):
        if len(arguments) == 1:
            owner = arguments[0].removeprefix("users/")
            barrier.wait()
            return {"type": "Organization" if owner == "org" else "User"}
        owner = arguments[-1].split("/")[1].split("?")[0]
        return [[
            {
                "full_name": f"{owner}/repo",
                "archived": False,
                "owner": {"login": owner},
            }
        ]]

    monkeypatch.setattr(github, "_gh_json", gh_json)

    assert github.discover(("person", "org")) == ["org/repo", "person/repo"]


def test_fix_cli_reports_failed_outcomes_as_nonzero(
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
    args = argparse.Namespace(
        repo=[], tag=[], rule=[], workers=1, push=False
    )

    monkeypatch.setattr(
        "repomod.cli.fix_repository",
        lambda *_: FixOutcome("acme/demo", cleanup_error="worktree remains"),
    )
    assert _fix(args) == 1
    assert "worktree remains" in capsys.readouterr().err

    monkeypatch.setattr(
        "repomod.cli.fix_repository",
        lambda *_: FixOutcome("acme/demo", skipped="checkout is dirty"),
    )
    args.repo = ["acme/demo"]
    assert _fix(args) == 1
    assert "checkout is dirty" in capsys.readouterr().err

    reference = "refs/repomod/recovery/acme/demo/1234"
    monkeypatch.setattr(
        "repomod.cli.fix_repository",
        lambda *_: FixOutcome(
            "acme/demo",
            commits=[("release", "b" * 40)],
            failures=[("publish", "injected merge failure")],
            recovery_ref=reference,
        ),
    )
    assert _fix(args) == 1
    output = capsys.readouterr().err
    assert "git -C" in output
    assert "merge --ff-only " + reference in output
    assert "update-ref -d " + reference in output


def test_warning_only_lint_is_nonfatal_unless_strict(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    repository = repo()
    catalog = Catalog(tmp_path / "catalog.toml", tmp_path, "acme", (repository,))
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    finding = Finding("acme/tool", "binary-release-tags", "release.yml", "review release", severity="warning")
    monkeypatch.setattr("repomod.cli.lint_repositories", lambda *args, **kwargs: [finding])
    args = argparse.Namespace(repo=[], tag=[], rule=[], json=False, strict=False)

    assert _lint(args) == 0
    assert "binary-release-tags review release" in capsys.readouterr().out
    args.strict = True
    assert _lint(args) == 1


def test_gha_settings_findings_carry_codes_locations_and_legend(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "ci.yml").write_text(
        "name: ci\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n"
        "    steps:\n      - run: make test\n",
        encoding="utf-8",
    )
    findings = _workflow_settings(tmp_path, repo())

    assert sorted(code for code in (finding.code for finding in findings) if code is not None) == [
        "G001",
        "G002",
        "G003",
    ]
    assert all(finding.file == ".github/workflows/ci.yml" for finding in findings)
    assert all(
        finding.line is not None and finding.line >= 1
        and finding.column is not None and finding.column >= 1
        for finding in findings
    )
    shorts = sorted(format_finding_short(finding) for finding in findings)
    assert shorts[0].endswith(" G001")
    assert shorts[1].endswith(" G002")
    assert shorts[2].endswith(" G003")
    assert all(short.startswith(".github/workflows/ci.yml ") for short in shorts)
    legend = lint_legend(findings)
    assert [code for code, _ in legend] == ["G001", "G002", "G003"]
    assert legend[0][1].startswith("concurrency.group must be")


def test_gha_settings_wrong_values_point_at_key_lines(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "ci.yml").write_text(
        "concurrency:\n  group: wrong\n  cancel-in-progress: false\n"
        "defaults:\n  run:\n    shell: sh\n",
        encoding="utf-8",
    )
    findings = {finding.code: finding for finding in _workflow_settings(tmp_path, repo())}

    assert set(findings) == {"G001", "G002", "G003"}
    assert (findings["G001"].line, findings["G001"].column) == (2, 3)
    assert (findings["G002"].line, findings["G002"].column) == (3, 3)
    assert (findings["G003"].line, findings["G003"].column) == (6, 5)


def test_gha_settings_findings_are_fixable_and_rule_has_fixer() -> None:
    rule = next(rule for rule in selected_rules(rule_ids=("gha-settings",)))

    assert rule.fix is not None
    assert rule.commit_message == "chore(repomod): enforce workflow settings"
    assert rule.allowed_paths == (".github/workflows/*.yml", ".github/workflows/*.yaml")


def test_gha_settings_fixer_inserts_missing_blocks_and_is_idempotent(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "ci.yml").write_text(
        "name: ci\non: push\njobs:\n  test:\n    runs-on: ubuntu-latest\n"
        "    steps:\n      - run: make test\n",
        encoding="utf-8",
    )
    repository = repo()
    before = _workflow_settings(tmp_path, repository)

    assert {finding.code for finding in before} == {"G001", "G002", "G003"}
    assert all(finding.fixable for finding in before)

    plan = _fix_workflow_settings(tmp_path, repository)

    assert list(plan) == [".github/workflows/ci.yml"]
    assert 'group: "${{ github.workflow }}-${{ github.head_ref || github.run_id }}"' in plan[
        ".github/workflows/ci.yml"
    ]
    assert "cancel-in-progress: true" in plan[".github/workflows/ci.yml"]
    assert "shell: bash -euo pipefail {0}" in plan[".github/workflows/ci.yml"]
    assert "      - run: make test\n" in plan[".github/workflows/ci.yml"]
    (workflows / "ci.yml").write_text(plan[".github/workflows/ci.yml"], encoding="utf-8")

    assert _workflow_settings(tmp_path, repository) == []
    assert _fix_workflow_settings(tmp_path, repository) == {}


def test_gha_settings_fixer_repairs_values_and_scalar_blocks(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "ci.yml").write_text(
        "name: ci\non: push\nconcurrency: shared\ndefaults:\n  run: sh\n"
        "jobs:\n  test:\n    runs-on: ubuntu-latest\n",
        encoding="utf-8",
    )
    (workflows / "other.yaml").write_text(
        "concurrency:\n  group: wrong\n  cancel-in-progress: false\n"
        "defaults:\n  run:\n    shell: sh\n",
        encoding="utf-8",
    )
    repository = repo()
    plan = _fix_workflow_settings(tmp_path, repository)

    assert sorted(plan) == [".github/workflows/ci.yml", ".github/workflows/other.yaml"]
    assert "concurrency: shared" not in plan[".github/workflows/ci.yml"]
    assert "  run: sh" not in plan[".github/workflows/ci.yml"]
    for relative, updated in plan.items():
        (tmp_path / relative).write_text(updated, encoding="utf-8")

    assert _workflow_settings(tmp_path, repository) == []
    assert _fix_workflow_settings(tmp_path, repository) == {}


def test_protection_reviews_reports_counts_and_skips_unprotected(monkeypatch) -> None:
    def gh_json(arguments: list[str], input_text: str | None = None):
        endpoint = arguments[0]
        name = endpoint.split("/")[2]
        if "protection" not in endpoint:
            return {"default_branch": "main"}
        if name == "full":
            return {"required_approving_review_count": 2}
        if name == "clean":
            return {"required_approving_review_count": 0}
        if name == "unprotected":
            raise github.GitHubError("Branch not protected (HTTP 404)")
        if name == "private":
            raise github.GitHubError("Upgrade to GitHub Pro (HTTP 403)")
        if name == "badcount":
            return {"required_approving_review_count": "many"}
        raise github.GitHubError("boom")

    monkeypatch.setattr(github, "_gh_json", gh_json)
    repositories = tuple(
        repo(name) for name in ("full", "clean", "unprotected", "private", "badcount", "broken")
    )

    results = {
        name: (protected, count, error)
        for name, protected, count, error in github.protection_many(repositories, jobs=1)
    }

    assert results["acme/full"] == (True, 2, None)
    assert results["acme/clean"] == (True, 0, None)
    assert results["acme/unprotected"] == (False, 0, None)
    assert results["acme/private"] == (False, 0, None)
    assert results["acme/badcount"][2] == "GitHub returned invalid review protection"
    assert results["acme/broken"][2] == "boom"


def test_protection_lint_flags_required_approvals(tmp_path: Path, monkeypatch) -> None:
    path = tmp_path / "catalog.toml"
    path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path)})
    monkeypatch.setattr(
        "repomod.policy.protection_many",
        lambda repositories: [(str(repository.id), True, 1, None) for repository in repositories],
    )

    findings = lint_repositories(catalog, rule_ids=("github-protection",))

    assert len(findings) == 1
    assert findings[0].rule == "github-protection"
    assert "must require none" in findings[0].message
    assert not findings[0].fixable


def test_protection_lint_skips_unprotected_and_reports_errors(
    tmp_path: Path, monkeypatch
) -> None:
    path = tmp_path / "catalog.toml"
    path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(path, {"HOME": str(tmp_path), "REPOMOD_ROOT": str(tmp_path)})
    monkeypatch.setattr(
        "repomod.policy.protection_many",
        lambda repositories: [(str(repository.id), False, 0, None) for repository in repositories],
    )

    assert lint_repositories(catalog, rule_ids=("github-protection",)) == []

    monkeypatch.setattr(
        "repomod.policy.protection_many",
        lambda repositories: [(str(repository.id), False, 0, "boom") for repository in repositories],
    )
    findings = lint_repositories(catalog, rule_ids=("github-protection",))

    assert len(findings) == 1
    assert findings[0].kind == "operational"
    assert "cannot read branch protection" in findings[0].message


def _init_default_branch(root: Path, branch: str = "main") -> None:
    subprocess.run(["git", "init", "-q"], cwd=root, check=True)
    subprocess.run(
        ["git", "remote", "add", "origin", "https://github.invalid/acme/demo.git"],
        cwd=root,
        check=True,
    )
    subprocess.run(
        ["git", "symbolic-ref", "refs/remotes/origin/HEAD", f"refs/remotes/origin/{branch}"],
        cwd=root,
        check=True,
    )


def test_gha_triggers_fixer_scopes_bare_push_to_default_branch(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "ci.yml").write_text(
        "name: ci\non:\n  push:\n  pull_request:\njobs:\n  test:\n    runs-on: ubuntu-latest\n",
        encoding="utf-8",
    )
    _init_default_branch(tmp_path)
    repository = repo()
    findings = _trigger_findings(tmp_path, repository)

    assert len(findings) == 1
    assert findings[0].rule == "gha-triggers"
    assert findings[0].fixable
    assert "[main]" in findings[0].message
    assert (findings[0].line, findings[0].column) == (3, 3)

    plan = _fix_triggers(tmp_path, repository)

    assert list(plan) == [".github/workflows/ci.yml"]
    assert "  push:\n    branches: [main]\n  pull_request:\n" in plan[".github/workflows/ci.yml"]
    (workflows / "ci.yml").write_text(plan[".github/workflows/ci.yml"], encoding="utf-8")

    assert _trigger_findings(tmp_path, repository) == []
    assert _fix_triggers(tmp_path, repository) == {}


def test_gha_triggers_accepts_scoped_push_and_single_triggers(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "scoped.yml").write_text(
        "on:\n  push:\n    branches: [main]\n  pull_request:\njobs: {}\n",
        encoding="utf-8",
    )
    (workflows / "pr-only.yml").write_text("on:\n  pull_request:\njobs: {}\n", encoding="utf-8")
    (workflows / "push-only.yml").write_text("on:\n  push:\njobs: {}\n", encoding="utf-8")
    _init_default_branch(tmp_path)

    assert _trigger_findings(tmp_path, repo()) == []
    assert _fix_triggers(tmp_path, repo()) == {}


def test_gha_triggers_handles_flow_on_block_and_unknown_default(tmp_path: Path) -> None:
    workflows = tmp_path / ".github" / "workflows"
    workflows.mkdir(parents=True)
    (workflows / "flow.yml").write_text(
        '---\nname: CI\n"on":\n  push: {}\n  pull_request: {}\njobs: {}\n',
        encoding="utf-8",
    )
    _init_default_branch(tmp_path, "trunk")
    repository = repo()
    findings = _trigger_findings(tmp_path, repository)

    assert len(findings) == 1
    assert findings[0].fixable

    plan = _fix_triggers(tmp_path, repository)

    assert "  push:\n    branches: [trunk]\n" in plan[".github/workflows/flow.yml"]

    plain = tmp_path / "plain"
    plain.mkdir()
    (plain / ".github" / "workflows").mkdir(parents=True)
    ((plain / ".github" / "workflows") / "ci.yml").write_text(
        "on:\n  push:\n  pull_request:\njobs: {}\n", encoding="utf-8"
    )
    subprocess.run(["git", "init", "-q"], cwd=plain, check=True)
    manual = _trigger_findings(plain, repository)

    assert len(manual) == 1
    assert not manual[0].fixable
    assert "default branch" in manual[0].message
    assert _fix_triggers(plain, repository) == {}


def test_lint_and_status_share_short_format_with_legend(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    repository = repo()
    catalog = Catalog(tmp_path / "catalog.toml", tmp_path, "acme", (repository,))
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    findings = [
        Finding("acme/tool", "gha-settings", ".github/workflows/ci.yml",
                "concurrency.group must be 'x'", line=2, column=1, code="G001"),
        Finding("acme/tool", "gha-settings", ".github/workflows/ci.yml",
                "defaults.run.shell must be 'y'", line=6, column=5, code="G003"),
    ]
    monkeypatch.setattr("repomod.cli.lint_repositories", lambda *args, **kwargs: findings)
    args = argparse.Namespace(repo=[], tag=[], rule=[], json=False, strict=False)

    assert _lint(args) == 1
    out = capsys.readouterr().out.splitlines()
    assert out[0] == "acme/tool .github/workflows/ci.yml 2:1 G001"
    assert out[1] == "acme/tool .github/workflows/ci.yml 6:5 G003"
    assert out[2] == "G001: concurrency.group must be 'x'"
    assert out[3] == "G003: defaults.run.shell must be 'y'"

    rows: list[dict[str, object]] = [{
        "repository": "acme/tool",
        "local": "clean",
        "lint": 2,
        "lint_errors": 2,
        "lint_warnings": 0,
        "lint_operational": 0,
        "todo_items": 0,
    }]
    ci = {"acme/tool": CiResult("acme/tool", None, None, "passed", (), workflows=())}
    _status_text(rows, {"acme/tool": findings}, ci)
    out = capsys.readouterr().out
    assert ".github/workflows/ci.yml 2:1 G001" in out
    assert ".github/workflows/ci.yml 6:5 G003" in out
    assert "G001: concurrency.group must be 'x'" in out
    assert "G003: defaults.run.shell must be 'y'" in out

    many = [
        Finding("acme/tool", "gha-settings", f"flow-{index}.yml", f"message {index}",
                line=index + 1, column=1, code="G001")
        for index in range(5)
    ]
    many_rows: list[dict[str, object]] = [{
        "repository": "acme/tool",
        "local": "clean",
        "lint": 5,
        "lint_errors": 5,
        "lint_warnings": 0,
        "lint_operational": 0,
        "todo_items": 0,
    }]
    _status_text(many_rows, {"acme/tool": many}, ci)
    out = capsys.readouterr().out

    for index in range(5):
        assert f"flow-{index}.yml {index + 1}:1 G001" in out
    assert "more (use --details" not in out


def test_operational_lint_failure_has_distinct_exit_code(
    tmp_path: Path, monkeypatch
) -> None:
    repository = repo()
    catalog = Catalog(tmp_path / "catalog.toml", tmp_path, "acme", (repository,))
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    finding = Finding("acme/tool", "repo-integrity", None, "checkout unavailable", kind="operational")
    monkeypatch.setattr("repomod.cli.lint_repositories", lambda *args, **kwargs: [finding])
    args = argparse.Namespace(repo=[], tag=[], rule=[], json=True, strict=False)

    assert _lint(args) == 2


def test_status_failed_excludes_warning_only_lint_findings(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ntool = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(catalog_path, {"HOME": str(tmp_path)})
    repository = catalog.repositories[0]
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    monkeypatch.setattr("repomod.cli.remote_metadata", lambda _: {})
    monkeypatch.setattr(
        "repomod.cli.ci_status",
        lambda repos, remotes, **kwargs: {
            str(repository.id): CiResult(str(repository.id), None, None, "passed", ())
        },
    )
    monkeypatch.setattr(
        "repomod.cli._checkout_status",
        lambda *_: (
            tmp_path / "tool",
            GitStatus("acme/tool", tmp_path / "tool", False, False, None, False, 0, 0, 0, None, None, None, False),
        ),
    )
    warning = Finding("acme/tool", "binary-release-tags", "release.yml", "review", severity="warning")
    monkeypatch.setattr("repomod.cli.lint_repositories", lambda *args, **kwargs: [warning])
    args = argparse.Namespace(
        repo=[], tag=[], offline=True, refresh=False, dirty=False,
        failed=True, json=True, details=False,
    )

    assert _status(args) == 0
    assert json.loads(capsys.readouterr().out) == []
    args.failed = False
    assert _status(args) == 0
    row = json.loads(capsys.readouterr().out)[0]
    assert row["lint"] == 1
    assert row["lint_warnings"] == 1
    assert row["lint_errors"] == 0


def test_open_pull_requests_reports_merge_state_and_invalid_data(monkeypatch) -> None:
    def graphql(query: str):
        assert "pullRequests(states: OPEN" in query
        return {
            "data": {
                "r0": {
                    "pullRequests": {
                        "nodes": [
                            {
                                "number": 3,
                                "url": "https://github.invalid/acme/demo/pull/3",
                                "mergeable": "CONFLICTING",
                                "mergeStateStatus": "BLOCKED",
                            },
                            {
                                "number": 4,
                                "url": "https://github.invalid/acme/demo/pull/4",
                                "mergeable": "MERGEABLE",
                                "mergeStateStatus": "CLEAN",
                            },
                        ]
                    }
                },
                "r1": {
                    "pullRequests": {
                        "nodes": [
                            {"number": "five", "url": "https://github.invalid/acme/other/pull/5"},
                        ]
                    }
                },
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)
    first, second = repo("demo"), repo("other")

    results = github.open_pull_requests((first, second))

    assert [request.number for request in results["acme/demo"][0]] == [3, 4]
    assert results["acme/demo"][0][0].mergeable == "CONFLICTING"
    assert results["acme/demo"][0][1].merge_state == "CLEAN"
    assert results["acme/demo"][1] is None
    assert results["acme/other"] == ([], "GitHub returned invalid pull request data")


def test_open_pull_requests_batch_errors_are_reported_per_repository(monkeypatch) -> None:
    def graphql(query: str):
        raise github.GitHubError("boom")

    monkeypatch.setattr(github, "_graphql", graphql)

    assert github.open_pull_requests((repo("demo"),)) == {"acme/demo": ([], "boom")}
    assert github.open_pull_requests(()) == {}


def test_status_failed_includes_conflicting_prs_and_renders_them(
    tmp_path: Path, monkeypatch, capsys
) -> None:
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(catalog_path, {"HOME": str(tmp_path)})
    repository = catalog.repositories[0]
    monkeypatch.setattr("repomod.cli._catalog", lambda: catalog)
    monkeypatch.setattr("repomod.cli.remote_metadata", lambda _: {})
    monkeypatch.setattr(
        "repomod.cli.ci_status",
        lambda repos, remotes, **kwargs: {
            str(repository.id): CiResult(str(repository.id), None, None, "passed", ())
        },
    )
    monkeypatch.setattr(
        "repomod.cli.open_pull_requests",
        lambda repositories: {
            str(repository.id): (
                [OpenPullRequest(3, "https://github.invalid/acme/demo/pull/3", "CONFLICTING", "BLOCKED")],
                None,
            )
        },
    )
    monkeypatch.setattr(
        "repomod.cli._checkout_status",
        lambda *_: (
            tmp_path / "tool",
            GitStatus("acme/tool", tmp_path / "tool", False, False, None, False, 0, 0, 0, None, None, None, False),
        ),
    )
    monkeypatch.setattr("repomod.cli.lint_repositories", lambda *args, **kwargs: [])
    args = argparse.Namespace(
        repo=[], tag=[], offline=False, refresh=False, dirty=False,
        failed=True, json=True, details=False,
    )

    assert _status(args) == 0
    rows = json.loads(capsys.readouterr().out)

    assert len(rows) == 1
    assert rows[0]["open_prs"] == [
        {
            "number": 3,
            "url": "https://github.invalid/acme/demo/pull/3",
            "mergeable": "CONFLICTING",
            "merge_state": "BLOCKED",
        }
    ]

    args.failed = False
    args.json = False
    assert _status(args) == 0
    assert "#3 CONFLICTING·BLOCKED" in capsys.readouterr().out

