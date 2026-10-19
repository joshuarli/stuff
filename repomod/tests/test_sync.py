"""Tests for repo sync: the activity filter and the lookups it avoids."""

import inspect
import json
import re
import subprocess
import threading
import time
from datetime import UTC, datetime, timedelta
from typing import Any
from unittest.mock import patch

import pytest

from repomod import sync
from repomod.types import Repo

NOW = datetime.now(UTC)


def _iso(days_ago: int) -> str:
    return (NOW - timedelta(days=days_ago)).strftime("%Y-%m-%dT%H:%M:%SZ")


def _commit(author: str, days_ago: int) -> dict[str, Any]:
    return {
        "committedDate": _iso(days_ago),
        "author": {"name": author, "email": None, "user": {"login": author}},
    }


def _history(*commits: dict[str, Any], more: bool = False) -> dict[str, Any]:
    return {"pageInfo": {"hasNextPage": more}, "nodes": list(commits)}


def _item(
    name: str, pushed_days_ago: int = 3, branch: str | None = "main"
) -> dict[str, Any]:
    return {
        "full_name": f"acme/{name}",
        "default_branch": branch,
        "topics": ["python", "rust"],
        "pushed_at": _iso(pushed_days_ago),
    }


class FakeGitHub:
    """Stands in for the two network calls sync makes, and records them."""

    def __init__(
        self,
        items: list[dict[str, Any]],
        histories: dict[str, dict[str, Any]] | None = None,
    ) -> None:
        self.items = items
        self.histories = histories or {}
        self.pages: list[int] = []
        self.history_batches: list[list[str]] = []
        self._lock = threading.Lock()
        self._running = 0
        self.max_concurrent = 0
        self.delay = 0.0

    def search_page(self, page: int) -> dict[str, Any]:
        with self._lock:
            self.pages.append(page)
        size = sync.SEARCH_PAGE_SIZE
        return {
            "total_count": len(self.items),
            "incomplete_results": False,
            "items": self.items[(page - 1) * size : page * size],
        }

    def graphql(self, query: str, **variables: str | None) -> dict[str, Any]:
        names = [
            f"{owner}/{repo}"
            for owner, repo in re.findall(r'owner: "([^"]+)", name: "([^"]+)"', query)
        ]
        with self._lock:
            self.history_batches.append(names)
            self._running += 1
            self.max_concurrent = max(self.max_concurrent, self._running)
        time.sleep(self.delay)
        with self._lock:
            self._running -= 1
        return {
            f"r{i}": {
                "defaultBranchRef": {
                    "target": {"history": self.histories.get(name, _history())}
                }
            }
            for i, name in enumerate(names)
        }

    @property
    def looked_up(self) -> set[str]:
        return {n for batch in self.history_batches for n in batch}

    def run(self, known: dict[str, Repo] | None = None) -> Any:
        with (
            patch.object(sync, "search_page", self.search_page),
            patch.object(sync, "graphql", self.graphql),
        ):
            return sync.sync_repos(known)


def test_is_bot_recognises_github_apps_and_org_bot_accounts() -> None:
    for name in ("dependabot[bot]", "release-bot", "Build Bot"):
        assert sync.is_bot({"name": name, "email": None, "user": None})


def test_is_bot_ignores_humans() -> None:
    human = {"name": "Ada Lovelace", "email": "ada@example.com", "user": {"login": "w"}}
    assert not sync.is_bot(human)


def test_cold_sync_applies_the_activity_filter_and_skips_provable_lookups() -> None:
    gh = FakeGitHub(
        items=[
            _item("relay"),
            _item("dependabot-only"),
            _item("long-bot-streak"),
            _item("milksnake", pushed_days_ago=800),
            _item("empty-repo", branch=None),
        ],
        histories={
            "acme/relay": _history(
                _commit("dependabot[bot]", 1), _commit("octocat", 9)
            ),
            "acme/dependabot-only": _history(_commit("dependabot[bot]", 2)),
            "acme/long-bot-streak": _history(
                _commit("renovate[bot]", 2), more=True
            ),
            "acme/empty-repo": _history(_commit("octocat", 5)),
        },
    )

    repos = gh.run()

    assert [r.name for r in repos] == [
        "acme/long-bot-streak",
        "acme/relay",
    ]
    # Not pushed to for two years: provably inactive, so its history isn't fetched.
    assert "acme/milksnake" not in gh.looked_up
    relay = repos[1]
    assert relay.default_branch == "main"
    assert relay.topics == ["python", "rust"]
    assert relay.last_human_commit == _iso(9)
    # All-bot first page with more history is kept but has no date to remember.
    assert repos[0].last_human_commit is None


def test_a_disabled_repo_is_not_synced() -> None:
    gh = FakeGitHub(
        items=[_item("relay"), _item("smolworld")],
        histories={
            "acme/relay": _history(_commit("octocat", 9)),
            "acme/smolworld": _history(_commit("octocat", 1)),
        },
    )

    with (
        patch.object(sync, "search_page", gh.search_page),
        patch.object(sync, "graphql", gh.graphql),
    ):
        repos = sync.sync_repos(disabled={"acme/smolworld"})

    assert [r.name for r in repos] == ["acme/relay"]
    assert "acme/smolworld" not in gh.looked_up


def test_disabled_repo_names_are_case_insensitive() -> None:
    gh = FakeGitHub(
        items=[
            {
                **_item("singlefile"),
                "full_name": "JoshuaRli/singlefile",
            }
        ],
        histories={"JoshuaRli/singlefile": _history(_commit("octocat", 1))},
    )

    with (
        patch.object(sync, "search_page", gh.search_page),
        patch.object(sync, "graphql", gh.graphql),
    ):
        repos = sync.sync_repos(disabled={"joshuarli/SingleFile"})

    assert repos == []
    assert "JoshuaRli/singlefile" not in gh.looked_up


def test_a_remembered_recent_human_commit_means_no_history_lookup_at_all() -> None:
    gh = FakeGitHub(items=[_item("relay"), _item("snuba"), _item("brand-new")])
    known = {
        "acme/relay": Repo("acme/relay", "main", [], _iso(40)),
        "acme/snuba": Repo("acme/snuba", "main", [], _iso(400)),
    }

    result = gh.run(known)

    # relay's remembered commit is inside the window; snuba's aged out, and
    # brand-new has no memory, so only those two are looked up.
    assert gh.looked_up == {"acme/snuba", "acme/brand-new"}
    by_name = {r.name: r for r in result}
    assert "acme/relay" in by_name
    assert by_name["acme/relay"].last_human_commit == _iso(40)


def test_a_fully_remembered_org_makes_no_history_calls() -> None:
    names = [f"service-{n}" for n in range(30)]
    gh = FakeGitHub(items=[_item(n) for n in names])
    known = {
        f"acme/{n}": Repo(f"acme/{n}", "main", [], _iso(10)) for n in names
    }

    result = gh.run(known)

    assert gh.history_batches == []
    assert len(result) == 30


def test_history_lookups_are_batched_and_run_concurrently() -> None:
    names = [f"service-{n}" for n in range(45)]
    gh = FakeGitHub(
        items=[_item(n) for n in names],
        histories={f"acme/{n}": _history(_commit("octocat", 3)) for n in names},
    )
    gh.delay = 0.1

    started = time.monotonic()
    result = gh.run()
    elapsed = time.monotonic() - started

    assert sorted(len(b) for b in gh.history_batches) == [5, 20, 20]
    assert gh.max_concurrent > 1
    assert elapsed < 0.25  # three 0.1s batches overlap instead of adding up
    assert len(result) == 45


def test_search_pages_after_the_first_are_fetched_together_and_deduplicated() -> None:
    items = [_item(f"service-{n}", pushed_days_ago=800) for n in range(250)]
    gh = FakeGitHub(items=items)
    original = gh.search_page

    def shifting(page: int) -> dict[str, Any]:
        payload = original(page)
        if page == 2:  # the index shifted between pages: page 1's last repo repeats
            payload["items"] = [items[99], *payload["items"]]
        return payload

    with (
        patch.object(sync, "search_page", shifting),
        patch.object(sync, "graphql", gh.graphql),
    ):
        result = sync.sync_repos()

    assert sorted(gh.pages) == [1, 2, 3]
    assert result == []


def test_a_partial_search_page_is_an_error_not_silently_truncated_repos() -> None:
    partial = json.dumps({"total_count": 294, "incomplete_results": True, "items": []})
    completed = subprocess.CompletedProcess([], 0, stdout=partial, stderr="")

    with (
        patch.object(sync.subprocess, "run", return_value=completed),
        pytest.raises(subprocess.CalledProcessError) as failure,
    ):
        inspect.unwrap(sync.search_page)(1)  # undecorated: no retry sleeps

    assert "incomplete results" in failure.value.stderr
