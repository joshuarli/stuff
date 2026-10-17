"""Fetch repo metadata from the GitHub API.

Syncing is two passes. A cheap listing gets every repo's basics (its pages are
independent, so they are fetched in parallel), then commit history (needed to drop
inactive repos) is fetched only for repos where it can't be ruled out some other
way, in parallel batches.
"""

import json
import math
import re
import subprocess
from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime, timedelta
from typing import Any

from repomod.types import OWNER_QUALIFIERS, Repo, SyncResult
from repomod.utils import SUBPROCESS_TIMEOUT, ShutdownError, interrupt, retry

# Appearing in the search doesn't mean a repo is maintained. Repos with no human
# commits on the default branch for this long are dropped to cut PR noise.
MAX_IDLE = timedelta(days=365)

# Bot authors: "dependabot[bot]", "release-bot", "Build Bot", etc.
BOT_NAME = re.compile(r"\[bot\]|[-\s]bot$", re.IGNORECASE)

# History is the expensive part, so it is requested in small batches: bigger ones
# (e.g. 50 repos x 50 commits) make GitHub return 502s. Batches run in parallel.
HISTORY_BATCH_SIZE = 20
HISTORY_WORKERS = 6
COMMITS_PER_REPO = 30

SEARCH_QUERY = f"{' '.join(OWNER_QUALIFIERS)} archived:false"
SEARCH_PAGE_SIZE = 100

HISTORY_FIELDS = f"""
  defaultBranchRef {{
    target {{
      ... on Commit {{
        history(first: {COMMITS_PER_REPO}, since: $since) {{
          pageInfo {{ hasNextPage }}
          nodes {{ committedDate author {{ name email user {{ login }} }} }}
        }}
      }}
    }}
  }}
"""


@retry()
def graphql(query: str, **variables: str | None) -> dict[str, Any]:
    """Run a GraphQL query through gh and return its `data`, with retry."""
    cmd = ["gh", "api", "graphql", "-f", f"query={query}"]
    for key, value in variables.items():
        if value is not None:
            cmd += ["-f", f"{key}={value}"]
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=SUBPROCESS_TIMEOUT,
            start_new_session=True,
        )
    except subprocess.TimeoutExpired as exc:
        if interrupt.is_set:
            raise ShutdownError from exc
        raise
    if result.returncode != 0:
        error = subprocess.CalledProcessError(
            result.returncode,
            cmd,
            result.stdout,
            result.stderr,
        )
        if interrupt.is_set:
            raise ShutdownError from error
        raise error
    try:
        return json.loads(result.stdout)["data"]  # type: ignore[no-any-return]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        if interrupt.is_set:
            raise ShutdownError from exc
        raise ValueError(f"Unexpected GraphQL response: {result.stdout[:200]}") from exc


@retry()
def search_page(page: int) -> dict[str, Any]:
    """One page of the REST repository search, with retry.

    REST search is paged by number rather than cursor, so pages don't depend on
    each other and can be fetched concurrently (GitHub's search backend is slow,
    several seconds per page, which made sequential paging the slowest part).
    """
    cmd = [
        "gh",
        "api",
        "--method",
        "GET",
        "search/repositories",
        "-f",
        f"q={SEARCH_QUERY}",
        "-f",
        f"per_page={SEARCH_PAGE_SIZE}",
        "-f",
        f"page={page}",
    ]
    try:
        result = subprocess.run(
            cmd,
            capture_output=True,
            text=True,
            timeout=SUBPROCESS_TIMEOUT,
            start_new_session=True,
        )
    except subprocess.TimeoutExpired as exc:
        if interrupt.is_set:
            raise ShutdownError from exc
        raise
    if result.returncode != 0:
        error = subprocess.CalledProcessError(
            result.returncode, cmd, result.stdout, result.stderr
        )
        if interrupt.is_set:
            raise ShutdownError from error
        raise error
    try:
        payload = json.loads(result.stdout)
        # GitHub sets this when the search timed out and the page may be partial.
        if payload["incomplete_results"]:
            error = subprocess.CalledProcessError(
                1, cmd, result.stdout, "search returned incomplete results"
            )
            if interrupt.is_set:
                raise ShutdownError from error
            raise error
        return payload  # type: ignore[no-any-return]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        if interrupt.is_set:
            raise ShutdownError from exc
        raise ValueError(f"Unexpected search response: {result.stdout[:200]}") from exc


def list_repos() -> list[dict[str, Any]]:
    """Every matching repo's basics (no commit history)."""
    first = search_page(1)
    pages = math.ceil(first["total_count"] / SEARCH_PAGE_SIZE)
    items: list[dict[str, Any]] = list(first["items"])
    if pages > 1:
        with ThreadPoolExecutor(max_workers=pages - 1) as pool:
            for payload in pool.map(search_page, range(2, pages + 1)):
                items += payload["items"]
    # Pages are fetched at different moments, so a repo can appear twice.
    return list({item["full_name"]: item for item in items}.values())


def is_bot(author: dict[str, Any]) -> bool:
    """Whether a commit author is an automated account."""
    login = (author.get("user") or {}).get("login") or ""
    return any(
        BOT_NAME.search(v or "")
        for v in (author.get("name"), author.get("email"), login)
    )


def judge_history(history: dict[str, Any] | None) -> tuple[bool, str | None]:
    """(keep, date of the newest non-bot commit) for a repo's recent history.

    Only the newest page of commits is inspected. If all of them are bots but
    there are more, we can't tell, so the repo is kept rather than wrongly
    dropped (and has no date to remember).
    """
    history = history or {}
    commits = history.get("nodes") or []
    for commit in commits:
        if not is_bot(commit["author"]):
            return True, str(commit["committedDate"])
    return bool(commits and history["pageInfo"]["hasNextPage"]), None


def parse_time(value: str) -> datetime:
    return datetime.fromisoformat(value)


def fetch_histories(names: list[str], cutoff: datetime) -> dict[str, dict[str, Any]]:
    """Recent default-branch history for repos, in parallel batches.

    Repos that come back with no history at all map to an empty dict.
    """
    since = cutoff.strftime("%Y-%m-%dT%H:%M:%SZ")
    batches = [
        names[i : i + HISTORY_BATCH_SIZE]
        for i in range(0, len(names), HISTORY_BATCH_SIZE)
    ]

    def fetch(batch: list[str]) -> dict[str, dict[str, Any]]:
        aliases = " ".join(
            f"r{i}: repository(owner: {json.dumps(name.split('/', 1)[0])}, "
            f"name: {json.dumps(name.split('/', 1)[1])}) {{ {HISTORY_FIELDS} }}"
            for i, name in enumerate(batch)
        )
        data = graphql(f"query($since: GitTimestamp) {{ {aliases} }}", since=since)
        histories: dict[str, dict[str, Any]] = {}
        for i, name in enumerate(batch):
            branch = (data.get(f"r{i}") or {}).get("defaultBranchRef") or {}
            histories[name] = (branch.get("target") or {}).get("history") or {}
        return histories

    histories: dict[str, dict[str, Any]] = {}
    with ThreadPoolExecutor(max_workers=HISTORY_WORKERS) as pool:
        for part in pool.map(fetch, batches):
            histories.update(part)
    return histories


def sync_repos(known: dict[str, Repo] | None = None) -> SyncResult:
    """Fetch non-archived repos owned by the configured owners that have had a
    non-bot default-branch commit within MAX_IDLE. Repos without are returned by
    name as excluded, so the filter can be audited.

    `known` is the previous sync's repos. Commit history is only fetched where
    it can't be ruled out:
      - not pushed to within the window => no commit in the window, so inactive;
      - last sync saw a human commit still inside the window => still active,
        since activity only ages out.
    """
    known = known or {}
    cutoff = datetime.now(UTC) - MAX_IDLE
    nodes = list_repos()

    verdicts: dict[str, tuple[bool, str | None]] = {}
    to_fetch: list[str] = []
    for node in nodes:
        name = node["full_name"]
        pushed = node.get("pushed_at")
        remembered = known[name].last_human_commit if name in known else None
        if not pushed or parse_time(pushed) < cutoff:
            verdicts[name] = (False, None)
        elif remembered is not None and parse_time(remembered) >= cutoff:
            verdicts[name] = (True, remembered)
        else:
            to_fetch.append(name)

    for name, history in fetch_histories(to_fetch, cutoff).items():
        verdicts[name] = judge_history(history)

    repos: list[Repo] = []
    excluded: list[str] = []
    for node in nodes:
        name = node["full_name"]
        keep, last_human_commit = verdicts[name]
        if not keep or not node.get("default_branch"):
            excluded.append(name)
            continue
        repos.append(
            Repo(
                name=name,
                default_branch=node["default_branch"],
                topics=sorted(node.get("topics") or []),
                last_human_commit=last_human_commit,
            )
        )

    repos.sort(key=lambda r: r.name)
    return SyncResult(repos=repos, excluded=sorted(excluded))
