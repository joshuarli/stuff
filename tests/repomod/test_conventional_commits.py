from __future__ import annotations

from pathlib import Path

from repomod import github
from repomod.catalog import load_catalog
from repomod.policy import lint_repositories, selected_rules
from repomod.rules.conventional_commits import _subject_problem, titles_findings
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def test_conventional_subjects_are_accepted() -> None:
    for title in (
        "fix(parser): handle empty input",
        "feat: add dark mode",
        "chore(repomod): enforce release and dist profiles",
        "docs: clarify the release contract",
        "refactor!: drop the legacy fallback",
        "test(ci): cover the new gate",
    ):
        assert _subject_problem(title) is None, title


def test_unconventional_subjects_are_rejected() -> None:
    for title in (
        "",
        "   ",
        "Vendor htmd and unify the stack",
        "WIP: things",
        "feat(): empty scope",
        "feat( ): blank scope",
        "feature: wrong type name",
        "FIX: shouting type",
        "fix missing colon",
        "fix: ",
        "fix: trailing period.",
    ):
        assert _subject_problem(title) is not None, title


def test_long_subject_is_a_warning_not_an_error(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    title = "fix(parser): " + "x" * 60
    assert len(title) > 72
    findings = titles_findings(
        repository("tool"), [(3, title, "https://github.invalid/acme/demo/pull/3", False)], None
    )
    assert len(findings) == 1
    assert findings[0].severity == "warning"


def test_drafts_are_skipped_and_errors_are_operational(tmp_path: Path) -> None:
    root = tmp_path / "demo"
    root.mkdir()
    findings = titles_findings(
        repository("tool"),
        [(3, "messy title", "https://github.invalid/acme/demo/pull/3", True)],
        None,
    )
    assert findings == []
    findings = titles_findings(repository("tool"), [], "boom")
    assert len(findings) == 1
    assert findings[0].kind == "operational"


def test_open_pull_requests_read_titles_and_draft_flags(monkeypatch) -> None:
    def graphql(query: str):
        assert "title" in query
        assert "isDraft" in query
        return {
            "data": {
                "r0": {
                    "pullRequests": {
                        "nodes": [
                            {
                                "number": 3,
                                "url": "https://github.invalid/acme/demo/pull/3",
                                "mergeable": "MERGEABLE",
                                "mergeStateStatus": "CLEAN",
                                "title": "fix: tidy",
                                "isDraft": True,
                            },
                        ]
                    }
                }
            }
        }

    monkeypatch.setattr(github, "_graphql", graphql)
    requests, error = github.open_pull_requests((repository("tool"),))["acme/demo"]
    assert error is None
    assert [(request.title, request.draft) for request in requests] == [("fix: tidy", True)]


def test_lint_reports_nonconventional_open_prs(tmp_path: Path, monkeypatch) -> None:
    catalog_path = tmp_path / "catalog.toml"
    catalog_path.write_text(
        'version = 1\n\n[workspace]\nroot = "~/repos"\npersonal_owner = "acme"\n\n'
        '[repos."acme"]\ndemo = ["tool"]\n',
        encoding="utf-8",
    )
    catalog = load_catalog(catalog_path, {"HOME": str(tmp_path)})
    (tmp_path / "demo").mkdir()

    def pulls(repositories):
        assert [str(repository.id) for repository in repositories] == ["acme/demo"]
        from repomod.types import OpenPullRequest

        return {
            "acme/demo": (
                [
                    OpenPullRequest(
                        3, "https://github.invalid/acme/demo/pull/3", "MERGEABLE", "CLEAN", "messy title", False
                    ),
                    OpenPullRequest(
                        4, "https://github.invalid/acme/demo/pull/4", "MERGEABLE", "CLEAN", "fix: tidy", False
                    ),
                ],
                None,
            )
        }

    monkeypatch.setattr("repomod.policy.open_pull_requests", pulls)
    findings = lint_repositories(catalog, rule_ids=("conventional-commits",))
    assert [(finding.rule, finding.message) for finding in findings] == [
        (
            "conventional-commits",
            "PR #3 https://github.invalid/acme/demo/pull/3: PR title 'messy title' is not a "
            "Conventional Commit subject (type[(scope)][!]: description with type in "
            "feat, fix, perf, refactor, docs, test, build, ci, chore, revert)",
        )
    ]


def test_rule_is_excluded_from_offline_checks() -> None:
    assert "conventional-commits" in {rule.id for rule in selected_rules()}
    assert "conventional-commits" not in {rule.id for rule in selected_rules(local_only=True)}


def test_rule_is_registered_with_lint_only_shape() -> None:
    rule = next(rule for rule in selected_rules() if rule.id == "conventional-commits")
    assert rule.fix is None
    assert rule.applies_to.matches(("tool",))
    assert rule.check(Path("."), repository("tool")) == []
