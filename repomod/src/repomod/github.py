from __future__ import annotations

import json
import math
import os
import re
import subprocess
import tempfile
import time
from collections.abc import Callable
from concurrent.futures import ThreadPoolExecutor, as_completed
from pathlib import Path
from typing import Any, cast

from repomod.types import (
    CheckResult,
    CiResult,
    CiState,
    RemoteRepository,
    RepoId,
    Repository,
)

API_TIMEOUT = 30
CACHE_TTL_SECONDS = 300
GRAPHQL_BATCH_SIZE = 20
MAX_API_WORKERS = 4
ACTION_TAG_PAGE_SIZE = 100
ACTION_TAG_RESOLUTION_DEPTH = 6
_TRANSIENT = ("http 429", "http 500", "http 502", "http 503", "http 504", "rate limit", "timed out")
_COMMIT_SHA = re.compile(r"[0-9a-f]{40}\Z", re.IGNORECASE)


class GitHubError(RuntimeError):
    pass


def _gh(arguments: list[str], input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    command = ["gh", "api", *arguments]
    last_error = "GitHub request failed"
    for attempt in range(3):
        try:
            result = subprocess.run(
                command,
                input=input_text,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=API_TIMEOUT,
                check=False,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            last_error = str(error)
            if attempt < 2:
                time.sleep(0.25 * (2**attempt))
                continue
            raise GitHubError(last_error) from error
        if result.returncode == 0:
            return result
        last_error = result.stderr.strip() or result.stdout.strip() or f"gh exited {result.returncode}"
        lowered = last_error.casefold()
        if attempt < 2 and any(marker in lowered for marker in _TRANSIENT):
            time.sleep(0.25 * (2**attempt))
            continue
        raise GitHubError(_safe_error(last_error))
    raise GitHubError(_safe_error(last_error))


def _safe_error(value: str) -> str:
    return re.sub(r"(https?://)[^/@\s]+@", r"\1[redacted]@", value)


def _gh_json(arguments: list[str], input_text: str | None = None) -> Any:
    result = _gh(arguments, input_text)
    try:
        return json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise GitHubError("GitHub returned malformed JSON") from error


def _graphql(query: str) -> dict[str, Any]:
    result = _gh_json(["graphql", "-f", f"query={query}"])
    if not isinstance(result, dict):
        raise GitHubError("GitHub GraphQL returned an invalid response")
    errors = result.get("errors")
    if errors and not isinstance(result.get("data"), dict):
        message = (
            errors[0].get("message", "GraphQL request failed")
            if isinstance(errors, list)
            else "GraphQL request failed"
        )
        raise GitHubError(str(message))
    return result


def _batches(items: tuple[Repository, ...], size: int = GRAPHQL_BATCH_SIZE) -> list[tuple[Repository, ...]]:
    return [items[offset : offset + size] for offset in range(0, len(items), size)]


def _repository_query(repositories: tuple[Repository, ...]) -> tuple[str, dict[str, str]]:
    aliases: dict[str, str] = {}
    fields: list[str] = []
    for index, repo in enumerate(repositories):
        alias = f"r{index}"
        aliases[alias] = str(repo.id)
        owner = json.dumps(repo.id.owner)
        name = json.dumps(repo.id.name)
        target = "target { ... on Commit { oid } }"
        fields.append(
            f'{alias}: repository(owner: {owner}, name: {name}) '
            f"{{ isArchived defaultBranchRef {{ name {target} }} }}"
        )
    return "query { " + " ".join(fields) + " }", aliases


def remote_metadata(
    repositories: tuple[Repository, ...],
    *,
    on_batch_complete: Callable[[int, int], None] | None = None,
) -> dict[str, RemoteRepository]:
    found: dict[str, RemoteRepository] = {}
    batches = _batches(repositories)
    if not batches:
        return found
    if len(batches) == 1:
        found = _remote_metadata_batch(batches[0])
        if on_batch_complete is not None:
            on_batch_complete(1, 1)
        return found
    completed = 0
    with ThreadPoolExecutor(
        max_workers=min(MAX_API_WORKERS, len(batches)), thread_name_prefix="repomod-github"
    ) as pool:
        for offset in range(0, len(batches), MAX_API_WORKERS):
            futures = [
                pool.submit(_remote_metadata_batch, batch)
                for batch in batches[offset : offset + MAX_API_WORKERS]
            ]
            for future in as_completed(futures):
                found.update(future.result())
                completed += 1
                if on_batch_complete is not None:
                    on_batch_complete(completed, len(batches))
    return found


def _remote_metadata_batch(batch: tuple[Repository, ...]) -> dict[str, RemoteRepository]:
    query, aliases = _repository_query(batch)
    found: dict[str, RemoteRepository] = {}
    try:
        data = _graphql(query).get("data", {})
    except GitHubError as error:
        return {
            name: RemoteRepository(name, None, None, None, str(error))
            for name in aliases.values()
        }
    for alias, name in aliases.items():
        repository = data.get(alias) if isinstance(data, dict) else None
        if not isinstance(repository, dict):
            found[name] = RemoteRepository(
                name, None, None, None, "repository not found or not accessible"
            )
            continue
        branch = repository.get("defaultBranchRef")
        if not isinstance(branch, dict):
            found[name] = RemoteRepository(
                name,
                None,
                None,
                bool(repository.get("isArchived")),
                "GitHub did not report a default branch",
            )
            continue
        target = branch.get("target")
        sha = target.get("oid") if isinstance(target, dict) else None
        found[name] = RemoteRepository(
            name,
            branch.get("name") if isinstance(branch.get("name"), str) else None,
            sha if isinstance(sha, str) and _COMMIT_SHA.fullmatch(sha) else None,
            repository.get("isArchived") if isinstance(repository.get("isArchived"), bool) else None,
        )
    return found


def _discover_owner(owner_name: str, owners: set[str]) -> list[str]:
    owner_metadata = _gh_json([f"users/{owner_name}"])
    owner_type = owner_metadata.get("type") if isinstance(owner_metadata, dict) else None
    endpoint = "orgs" if owner_type == "Organization" else "users"
    pages = _gh_json(
        ["--paginate", "--slurp", f"{endpoint}/{owner_name}/repos?type=all&per_page=100"]
    )
    if not isinstance(pages, list):
        raise GitHubError(f"GitHub returned an invalid repository list for {owner_name}")
    repositories: list[str] = []
    for page in pages:
        if not isinstance(page, list):
            raise GitHubError(f"GitHub returned an invalid repository page for {owner_name}")
        for item in page:
            if not isinstance(item, dict):
                continue
            full_name = item.get("full_name")
            archived = item.get("archived")
            owner = item.get("owner")
            login = owner.get("login") if isinstance(owner, dict) else None
            if (
                isinstance(full_name, str)
                and isinstance(login, str)
                and login.casefold() in owners
                and archived is False
            ):
                repositories.append(full_name)
    return repositories


def discover(owner_names: tuple[str, ...]) -> list[str]:
    if not owner_names:
        return []
    owners = {owner.casefold() for owner in owner_names}
    if len(owner_names) == 1:
        return sorted(set(_discover_owner(owner_names[0], owners)), key=str.casefold)
    repositories: list[str] = []
    with ThreadPoolExecutor(
        max_workers=min(MAX_API_WORKERS, len(owner_names)), thread_name_prefix="repomod-discover"
    ) as pool:
        futures = [pool.submit(_discover_owner, owner, owners) for owner in owner_names]
        for future in as_completed(futures):
            repositories.extend(future.result())
    return sorted(set(repositories), key=str.casefold)


def _state_for_checks(checks: tuple[CheckResult, ...], complete: bool) -> CiState:
    if not complete:
        return "unknown"
    latest_statuses: dict[tuple[str, str], CheckResult] = {}
    attempts: dict[str, int] = {}
    for check in checks:
        if check.check_suite_id is not None and check.run_attempt is not None:
            identity = check.workflow_run_id or check.check_suite_id
            attempts[identity] = max(
                check.run_attempt, attempts.get(identity, 0)
            )
    independent_checks: list[CheckResult] = []
    severity = {
        "failure": 5,
        "failed": 5,
        "error": 5,
        "timed_out": 5,
        "action_required": 5,
        "startup_failure": 5,
        "queued": 4,
        "requested": 4,
        "waiting": 4,
        "pending": 4,
        "expected": 4,
        "in_progress": 4,
        "cancelled": 3,
    }
    for check in checks:
        if check.source is not None and check.source.startswith("status:"):
            if check.updated_at is None:
                independent_checks.append(check)
                continue
            key = (check.name.casefold(), check.source.casefold())
            previous = latest_statuses.get(key)
            newer = (
                previous is None
                or previous.updated_at is None
                or check.updated_at > previous.updated_at
            )
            same_time_failure = (
                previous is not None
                and check.updated_at == previous.updated_at
                and severity.get(check.state.casefold(), 0)
                > severity.get(previous.state.casefold(), 0)
            )
            if newer or same_time_failure:
                latest_statuses[key] = check
            continue
        if (
            check.check_suite_id is not None
            and check.run_attempt is not None
            and check.run_attempt
            < attempts[check.workflow_run_id or check.check_suite_id]
        ):
            continue
        independent_checks.append(check)
    current = (*latest_statuses.values(), *independent_checks)
    if not current:
        return "none"
    states = {check.state.casefold() for check in current}
    if states & {"failure", "failed", "error", "timed_out", "action_required", "startup_failure"}:
        return "failed"
    if states & {"queued", "requested", "waiting", "pending", "expected", "in_progress"}:
        return "pending"
    if "cancelled" in states:
        return "cancelled"
    if states <= {"skipped", "neutral"}:
        return "neutral" if states == {"neutral"} else "skipped"
    if states <= {"success", "passed", "neutral", "skipped"}:
        return "passed"
    if "stale" in states:
        return "stale"
    return "unknown"


def _check_result(node: dict[str, Any]) -> CheckResult | None:
    kind = node.get("__typename")
    check_suite_id = None
    workflow_run_id = None
    run_attempt = None
    if kind == "CheckRun":
        name = node.get("name")
        status_value = node.get("status")
        conclusion = node.get("conclusion")
        state = conclusion if isinstance(conclusion, str) else status_value
        url = node.get("detailsUrl")
        app = node.get("app")
        source = app.get("name") if isinstance(app, dict) else None
        updated_at = node.get("completedAt") or node.get("startedAt")
        suite = node.get("checkSuite")
        check_suite_id = suite.get("id") if isinstance(suite, dict) else None
        workflow_run = suite.get("workflowRun") if isinstance(suite, dict) else None
        workflow_run_id = workflow_run.get("id") if isinstance(workflow_run, dict) else None
        run_attempt = workflow_run.get("runAttempt") if isinstance(workflow_run, dict) else None
    elif kind == "StatusContext":
        name = node.get("context")
        state = node.get("state")
        url = node.get("targetUrl")
        creator = node.get("creator")
        creator_name = creator.get("login") if isinstance(creator, dict) else None
        source = f"status:{creator_name}" if isinstance(creator_name, str) else "status:unknown"
        updated_at = node.get("updatedAt") or node.get("createdAt")
    else:
        return None
    if not isinstance(name, str) or not isinstance(state, str):
        return None
    return CheckResult(
        name,
        state.casefold(),
        url if isinstance(url, str) else None,
        source if isinstance(source, str) else None,
        updated_at if isinstance(updated_at, str) else None,
        check_suite_id if isinstance(check_suite_id, str) else None,
        workflow_run_id if isinstance(workflow_run_id, str) else None,
        run_attempt if isinstance(run_attempt, int) else None,
    )


def _status_check_page(
    pending: dict[str, tuple[RepoId, str]],
) -> tuple[dict[str, tuple[tuple[CheckResult, ...], bool]], dict[str, str]]:
    collected: dict[str, list[CheckResult]] = {alias: [] for alias in pending}
    cursors = cast(dict[str, str | None], dict.fromkeys(pending))
    complete = dict.fromkeys(pending, False)
    errors: dict[str, str] = {}
    page = 0
    while any(
        not complete[alias] and alias not in errors for alias in pending
    ) and page < 20:
        fields: list[str] = []
        active: dict[str, str] = {}
        for alias, (repo_id, sha) in pending.items():
            if complete[alias] or alias in errors:
                continue
            owner = json.dumps(repo_id.owner)
            name = json.dumps(repo_id.name)
            cursor = cursors[alias]
            after = f', after: {json.dumps(cursor)}' if cursor is not None else ""
            fields.append(
                f'{alias}: repository(owner: {owner}, name: {name}) '
                f'{{ object(oid: "{sha}") {{ ... on Commit {{ statusCheckRollup '
                f'{{ contexts(first: 100{after}) {{ nodes {{ __typename '
                f'... on CheckRun {{ name app {{ name }} status conclusion '
                f'detailsUrl startedAt completedAt checkSuite {{ id workflowRun '
                f'{{ id runAttempt }} }} }} '
                f'... on StatusContext {{ context state targetUrl createdAt updatedAt '
                f'creator {{ login }} }} }} '
                f'pageInfo {{ hasNextPage endCursor }} }} }} }} }}'
            )
            active[alias] = alias
        if not fields:
            break
        try:
            data = _graphql("query { " + " ".join(fields) + " }").get("data", {})
        except GitHubError as error:
            errors.update({alias: str(error) for alias in active})
            break
        for alias in active:
            repository = data.get(alias) if isinstance(data, dict) else None
            obj = repository.get("object") if isinstance(repository, dict) else None
            rollup = obj.get("statusCheckRollup") if isinstance(obj, dict) else None
            if not isinstance(obj, dict):
                errors[alias] = "GitHub did not return the requested commit"
                continue
            if rollup is None:
                complete[alias] = True
                continue
            connection = rollup.get("contexts") if isinstance(rollup, dict) else None
            if not isinstance(connection, dict):
                errors[alias] = "GitHub did not return check contexts"
                continue
            for node in connection.get("nodes", []):
                if isinstance(node, dict):
                    check = _check_result(node)
                    if check is not None:
                        collected[alias].append(check)
            page_info = connection.get("pageInfo")
            if not isinstance(page_info, dict):
                errors[alias] = "GitHub omitted check pagination metadata"
                continue
            if page_info.get("hasNextPage") is True:
                cursor = page_info.get("endCursor")
                if not isinstance(cursor, str) or not cursor:
                    errors[alias] = "GitHub returned an invalid check pagination cursor"
                else:
                    cursors[alias] = cursor
            else:
                complete[alias] = True
        page += 1
    for alias in pending:
        if not complete[alias] and alias not in errors:
            errors[alias] = "GitHub check pagination exceeded 2,000 results"
    return (
        {
            alias: (
                tuple(
                    sorted(
                        checks,
                        key=lambda item: (
                            item.name.casefold(),
                            item.state,
                            item.updated_at or "",
                        ),
                    )
                ),
                complete[alias],
            )
            for alias, checks in collected.items()
        },
        errors,
    )


def _status_check_pages(
    pending: dict[str, tuple[RepoId, str]],
) -> tuple[dict[str, tuple[tuple[CheckResult, ...], bool]], dict[str, str]]:
    items = tuple(pending.items())
    results: dict[str, tuple[tuple[CheckResult, ...], bool]] = {}
    errors: dict[str, str] = {}
    batches = [
        dict(items[offset : offset + GRAPHQL_BATCH_SIZE])
        for offset in range(0, len(items), GRAPHQL_BATCH_SIZE)
    ]
    if not batches:
        return results, errors
    if len(batches) == 1:
        return _status_check_page(batches[0])
    with ThreadPoolExecutor(
        max_workers=min(MAX_API_WORKERS, len(batches)), thread_name_prefix="repomod-ci"
    ) as pool:
        for offset in range(0, len(batches), MAX_API_WORKERS):
            futures = [
                pool.submit(_status_check_page, batch)
                for batch in batches[offset : offset + MAX_API_WORKERS]
            ]
            for future in as_completed(futures):
                checks, failures = future.result()
                results.update(checks)
                errors.update(failures)
    return results, errors


def _cache_path() -> Path:
    cache_root = Path(os.environ.get("XDG_CACHE_HOME", Path.home() / ".cache")).expanduser()
    return cache_root / "repomod" / "ci.json"


def _read_cache(path: Path) -> tuple[dict[str, dict[str, Any]], str | None]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return {}, None
    except (OSError, json.JSONDecodeError) as error:
        return {}, f"CI cache is unreadable: {error}"
    if not isinstance(data, dict) or data.get("version") != 1 or not isinstance(data.get("entries"), dict):
        return {}, "CI cache has an unsupported format"
    return {key: value for key, value in data["entries"].items() if isinstance(value, dict)}, None


def _write_cache(path: Path, entries: dict[str, dict[str, Any]]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor, temporary_name = tempfile.mkstemp(prefix=".ci.", dir=path.parent)
    temporary_path = Path(temporary_name)
    try:
        with os.fdopen(descriptor, "w", encoding="utf-8", newline="\n") as output:
            json.dump({"version": 1, "entries": entries}, output, sort_keys=True, separators=(",", ":"))
            output.write("\n")
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary_path, path)
    except BaseException:
        temporary_path.unlink(missing_ok=True)
        raise


def _cached_for(entries: dict[str, dict[str, Any]], name: str) -> tuple[str, dict[str, Any]] | None:
    prefix = f"{name}@"
    candidates = [
        (key, item)
        for key, item in entries.items()
        if key.startswith(prefix)
        and isinstance(item.get("fetched_at"), (int, float))
        and not isinstance(item.get("fetched_at"), bool)
        and math.isfinite(item["fetched_at"])
    ]
    if not candidates:
        return None
    return max(candidates, key=lambda pair: float(pair[1].get("fetched_at", 0)))


def _result_from_cache(
    repository: str,
    entry: dict[str, Any],
    now: float,
    *,
    stale: bool = False,
    error: str | None = None,
) -> CiResult:
    raw_checks = entry.get("checks", [])
    if not isinstance(raw_checks, list):
        raw_checks = []
    checks = tuple(
        CheckResult(
            item["name"],
            item["state"],
            item.get("url") if isinstance(item.get("url"), str) else None,
            item.get("source") if isinstance(item.get("source"), str) else None,
            item.get("updated_at") if isinstance(item.get("updated_at"), str) else None,
        )
        for item in raw_checks
        if isinstance(item, dict) and isinstance(item.get("name"), str) and isinstance(item.get("state"), str)
    )
    fetched = entry.get("fetched_at")
    fetched_at = (
        fetched
        if isinstance(fetched, (int, float))
        and not isinstance(fetched, bool)
        and math.isfinite(fetched)
        else now
    )
    age = max(0, int(now - fetched_at))
    state = "stale" if stale else entry.get("state", "unknown")
    if state not in {"passed", "failed", "pending", "cancelled", "skipped", "neutral", "none", "unknown", "stale"}:
        state = "unknown"
    return CiResult(
        repository,
        entry.get("default_branch") if isinstance(entry.get("default_branch"), str) else None,
        entry.get("sha") if isinstance(entry.get("sha"), str) else None,
        state,
        checks,
        True,
        age,
        error,
    )


def ci_status(
    repositories: tuple[Repository, ...],
    remotes: dict[str, RemoteRepository] | None = None,
    *,
    offline: bool = False,
    refresh: bool = False,
    cache_path: Path | None = None,
    now: float | None = None,
) -> dict[str, CiResult]:
    current_time = time.time() if now is None else now
    path = cache_path or _cache_path()
    cache, cache_error = _read_cache(path)
    if offline:
        results: dict[str, CiResult] = {}
        for repo in repositories:
            name = str(repo.id)
            cached = None if refresh else _cached_for(cache, name)
            if cached:
                results[name] = _result_from_cache(name, cached[1], current_time)
            else:
                results[name] = CiResult(name, None, None, "unknown", (), error=cache_error or "no cached CI result")
        return results

    remote_values = remote_metadata(repositories) if remotes is None else remotes
    results = {}
    query: list[Repository] = []
    selected: dict[str, tuple[Repository, RemoteRepository, dict[str, Any] | None]] = {}
    for repo in repositories:
        name = str(repo.id)
        remote = remote_values.get(name)
        cached_pair = _cached_for(cache, name)
        cached = cached_pair[1] if cached_pair else None
        if (
            remote is None
            or remote.error is not None
            or remote.default_branch is None
            or remote.sha is None
            or _COMMIT_SHA.fullmatch(remote.sha) is None
        ):
            remote_error = (
                remote.error
                if remote is not None and remote.error is not None
                else "current remote default branch or HEAD is unavailable"
            )
            if cached is not None:
                results[name] = _result_from_cache(
                    name, cached, current_time, stale=True, error=remote_error
                )
            else:
                results[name] = CiResult(name, None, None, "unknown", (), error=remote_error)
            continue
        if (
            cached is not None
            and not refresh
            and cached.get("sha") == remote.sha
            and isinstance(cached.get("fetched_at"), (int, float))
            and not isinstance(cached.get("fetched_at"), bool)
            and math.isfinite(cached["fetched_at"])
            and current_time - cached["fetched_at"] < CACHE_TTL_SECONDS
        ):
            results[name] = _result_from_cache(name, cached, current_time)
            continue
        query.append(repo)
        selected[name] = (repo, remote, cached)

    pending = {
        f"r{index}": (repo.id, remote.sha or "")
        for index, repo in enumerate(query)
        if (remote := remote_values.get(str(repo.id))) is not None and remote.sha is not None
    }
    if pending:
        try:
            pages, errors = _status_check_pages(pending)
        except GitHubError as error:
            pages = {}
            errors = {alias: str(error) for alias in pending}
        fetched: dict[str, dict[str, Any]] = {}
        for index, repo in enumerate(query):
            alias = f"r{index}"
            name = str(repo.id)
            _, remote, old_cache = selected[name]
            page_data = pages.get(alias)
            error = errors.get(alias)
            if page_data is None or error is not None:
                if old_cache is not None:
                    results[name] = _result_from_cache(
                        name,
                        old_cache,
                        current_time,
                        stale=True,
                        error=error or "check rollup unavailable",
                    )
                else:
                    results[name] = CiResult(
                        name,
                        remote.default_branch,
                        remote.sha,
                        "unknown",
                        (),
                        error=error or "check rollup unavailable",
                    )
                continue
            checks, complete = page_data
            state = _state_for_checks(checks, complete)
            result = CiResult(
                name,
                remote.default_branch,
                remote.sha,
                state,
                checks,
                False,
                0,
                None if state != "unknown" else "check rollup was incomplete",
            )
            results[name] = result
            fetched[f"{name}@{remote.sha}"] = {
                "default_branch": remote.default_branch,
                "sha": remote.sha,
                "state": state,
                "checks": [
                    {
                        "name": check.name,
                        "state": check.state,
                        "url": check.url,
                        "source": check.source,
                        "updated_at": check.updated_at,
                    }
                    for check in checks
                ],
                "fetched_at": current_time,
            }
        if fetched:
            cache.update(fetched)
            _write_cache(path, cache)
    return results


def _topics_get(repo: RepoId) -> tuple[str, ...]:
    data = _gh_json([f"repos/{repo.owner}/{repo.name}/topics"])
    names = data.get("names") if isinstance(data, dict) else None
    if not isinstance(names, list) or any(not isinstance(name, str) for name in names):
        raise GitHubError(f"GitHub returned invalid topics for {repo}")
    valid_names = [name for name in names if isinstance(name, str)]
    return tuple(sorted(set(valid_names), key=str.casefold))


def topics_diff(repo: Repository) -> tuple[tuple[str, ...], tuple[str, ...]]:
    actual = set(_topics_get(repo.id))
    desired = set(repo.tags)
    return tuple(sorted(desired - actual)), tuple(sorted(actual - desired))


def topics_apply(repo: Repository) -> tuple[bool, tuple[str, ...], tuple[str, ...]]:
    additions, removals = topics_diff(repo)
    if not additions and not removals:
        return False, additions, removals
    body = json.dumps({"names": list(repo.tags)}, separators=(",", ":"))
    _gh_json(
        [
            f"repos/{repo.id.owner}/{repo.id.name}/topics",
            "--method",
            "PUT",
            "--input",
            "-",
        ],
        body,
    )
    return True, additions, removals


type TopicResult = tuple[str, bool, tuple[str, ...], tuple[str, ...], str | None]


def _topics_query(repositories: tuple[Repository, ...]) -> tuple[str, dict[str, Repository]]:
    aliases: dict[str, Repository] = {}
    fields: list[str] = []
    for index, repository in enumerate(repositories):
        alias = f"r{index}"
        aliases[alias] = repository
        owner = json.dumps(repository.id.owner)
        name = json.dumps(repository.id.name)
        fields.append(
            f'{alias}: repository(owner: {owner}, name: {name}) '
            "{ repositoryTopics(first: 20) { nodes { topic { name } } } }"
        )
    return "query { " + " ".join(fields) + " }", aliases


def _topics_diff_batch(repositories: tuple[Repository, ...]) -> list[TopicResult]:
    query, aliases = _topics_query(repositories)

    def failed(message: str) -> list[TopicResult]:
        return [(str(repository.id), False, (), (), message) for repository in repositories]

    try:
        response = _graphql(query)
    except GitHubError as error:
        return failed(str(error))
    data = response.get("data")
    if not isinstance(data, dict):
        return failed("GitHub returned invalid topic data")

    errors = response.get("errors")
    alias_errors: dict[str, list[str]] = {}
    global_errors: list[str] = []
    if errors:
        if not isinstance(errors, list):
            global_errors.append("GitHub returned invalid GraphQL topic errors")
        else:
            for error in errors:
                if not isinstance(error, dict):
                    global_errors.append("GitHub returned invalid GraphQL topic errors")
                    continue
                message = error.get("message")
                detail = _safe_error(message) if isinstance(message, str) else "GraphQL topic query failed"
                path = error.get("path")
                alias = path[0] if isinstance(path, list) and path else None
                if isinstance(alias, str) and alias in aliases:
                    alias_errors.setdefault(alias, []).append(detail)
                else:
                    global_errors.append(detail)
    if global_errors:
        return failed("; ".join(dict.fromkeys(global_errors)))

    results: list[TopicResult] = []
    for alias, repository in aliases.items():
        if alias in alias_errors:
            results.append(
                (str(repository.id), False, (), (), "; ".join(dict.fromkeys(alias_errors[alias])))
            )
            continue
        value = data.get(alias)
        if not isinstance(value, dict):
            results.append(
                (str(repository.id), False, (), (), "repository not found or not accessible")
            )
            continue
        topic_data = value.get("repositoryTopics")
        nodes = topic_data.get("nodes") if isinstance(topic_data, dict) else None
        if not isinstance(nodes, list):
            results.append((str(repository.id), False, (), (), "GitHub returned invalid topics"))
            continue
        names: list[str] | None = []
        for node in nodes:
            topic = node.get("topic") if isinstance(node, dict) else None
            name = topic.get("name") if isinstance(topic, dict) else None
            if not isinstance(name, str):
                names = None
                break
            names.append(name)
        if names is None:
            results.append((str(repository.id), False, (), (), "GitHub returned invalid topics"))
            continue
        actual = set(names)
        desired = set(repository.tags)
        additions = tuple(sorted(desired - actual, key=str.casefold))
        removals = tuple(sorted(actual - desired, key=str.casefold))
        results.append((str(repository.id), False, additions, removals, None))
    return results


def _topics_diff_many(repositories: tuple[Repository, ...], jobs: int) -> list[TopicResult]:
    batches = _batches(repositories)
    if not batches:
        return []
    output: list[TopicResult] = []
    with ThreadPoolExecutor(
        max_workers=min(jobs, 16, len(batches)), thread_name_prefix="repomod-topics"
    ) as pool:
        futures = [pool.submit(_topics_diff_batch, batch) for batch in batches]
        for future in as_completed(futures):
            output.extend(future.result())
    return sorted(output, key=lambda item: item[0].casefold())


def topics_many(
    repositories: tuple[Repository, ...], *, apply: bool, jobs: int = 4
) -> list[TopicResult]:
    if jobs < 1:
        raise ValueError("jobs must be at least one")
    if not apply:
        return _topics_diff_many(repositories, jobs)

    def one(repo: Repository) -> TopicResult:
        try:
            changed, additions, removals = topics_apply(repo)
            return str(repo.id), changed, additions, removals, None
        except GitHubError as error:
            return str(repo.id), False, (), (), str(error)

    if not repositories:
        return []
    output: list[TopicResult] = []
    with ThreadPoolExecutor(max_workers=min(jobs, 16, len(repositories)), thread_name_prefix="repomod-topics") as pool:
        futures = [pool.submit(one, repo) for repo in repositories]
        output = [future.result() for future in as_completed(futures)]
    return sorted(output, key=lambda item: item[0].casefold())


def _graphql_batch_data(response: dict[str, Any], aliases: dict[str, str]) -> dict[str, Any]:
    errors = response.get("errors")
    if errors:
        alias_errors: dict[str, list[str]] = {}
        global_errors: list[str] = []
        if not isinstance(errors, list):
            global_errors.append("GitHub returned invalid GraphQL errors")
        else:
            for error in errors:
                if not isinstance(error, dict):
                    global_errors.append("GitHub returned invalid GraphQL errors")
                    continue
                message = error.get("message")
                detail = (
                    _safe_error(message)
                    if isinstance(message, str)
                    else "GraphQL request failed"
                )
                path = error.get("path")
                alias = path[0] if isinstance(path, list) and path else None
                if isinstance(alias, str) and alias in aliases:
                    alias_errors.setdefault(alias, []).append(detail)
                else:
                    global_errors.append(detail)
        if global_errors:
            raise GitHubError("; ".join(dict.fromkeys(global_errors)))
        raise GitHubError(
            "; ".join(
                f"{aliases[alias]}: {'; '.join(dict.fromkeys(messages))}"
                for alias, messages in alias_errors.items()
            )
        )
    data = response.get("data")
    if not isinstance(data, dict):
        raise GitHubError("GitHub returned invalid GraphQL data")
    return data


def _action_repository_parts(repository: str) -> tuple[str, str]:
    owner, separator, name = repository.partition("/")
    if not owner or not separator or not name or "/" in name:
        raise GitHubError(f"invalid GitHub Action repository {repository}")
    return owner, name


def _action_tags_batch(repositories: tuple[str, ...], prefix: str) -> dict[str, tuple[str, ...]]:
    aliases = {f"a{index}": repository for index, repository in enumerate(repositories)}
    cursors = cast(dict[str, str | None], dict.fromkeys(aliases))
    seen_cursors: dict[str, set[str]] = {alias: set() for alias in aliases}
    complete = dict.fromkeys(aliases, False)
    tags: dict[str, list[str]] = {alias: [] for alias in aliases}
    while any(not complete[alias] for alias in aliases):
        fields: list[str] = []
        for alias, repository in aliases.items():
            if complete[alias]:
                continue
            owner, name = _action_repository_parts(repository)
            owner_value = json.dumps(owner)
            name_value = json.dumps(name)
            prefix_value = json.dumps(f"refs/tags/{prefix}")
            cursor = cursors[alias]
            after = f", after: {json.dumps(cursor)}" if cursor is not None else ""
            fields.append(
                f'{alias}: repository(owner: {owner_value}, name: {name_value}) '
                f'{{ refs(first: {ACTION_TAG_PAGE_SIZE}, refPrefix: {prefix_value}{after}) '
                "{ nodes { name } pageInfo { hasNextPage endCursor } } }"
            )
        data = _graphql_batch_data(
            _graphql("query { " + " ".join(fields) + " }"), aliases
        )
        for alias, repository in aliases.items():
            if complete[alias]:
                continue
            value = data.get(alias)
            refs = value.get("refs") if isinstance(value, dict) else None
            nodes = refs.get("nodes") if isinstance(refs, dict) else None
            page_info = refs.get("pageInfo") if isinstance(refs, dict) else None
            if not isinstance(nodes, list) or not isinstance(page_info, dict):
                raise GitHubError(f"GitHub returned invalid tag refs for {repository}")
            for node in nodes:
                tag = node.get("name") if isinstance(node, dict) else None
                if not isinstance(tag, str):
                    raise GitHubError(f"GitHub returned an invalid tag name for {repository}")
                tags[alias].append(tag)
            has_next_page = page_info.get("hasNextPage")
            if not isinstance(has_next_page, bool):
                raise GitHubError(f"GitHub omitted tag pagination state for {repository}")
            if not has_next_page:
                complete[alias] = True
                continue
            cursor = page_info.get("endCursor")
            if not isinstance(cursor, str) or not cursor or cursor in seen_cursors[alias]:
                raise GitHubError(f"GitHub returned an invalid tag pagination cursor for {repository}")
            seen_cursors[alias].add(cursor)
            cursors[alias] = cursor
    return {
        repository: tuple(tags[alias])
        for alias, repository in aliases.items()
    }


def action_tags_many(repositories: tuple[str, ...], prefix: str) -> dict[str, tuple[str, ...]]:
    batches = [
        repositories[offset : offset + GRAPHQL_BATCH_SIZE]
        for offset in range(0, len(repositories), GRAPHQL_BATCH_SIZE)
    ]
    found: dict[str, tuple[str, ...]] = {}
    if not batches:
        return found
    if len(batches) == 1:
        return _action_tags_batch(batches[0], prefix)
    with ThreadPoolExecutor(
        max_workers=min(MAX_API_WORKERS, len(batches)), thread_name_prefix="repomod-action-refs"
    ) as pool:
        for offset in range(0, len(batches), MAX_API_WORKERS):
            futures = [
                pool.submit(_action_tags_batch, batch, prefix)
                for batch in batches[offset : offset + MAX_API_WORKERS]
            ]
            for future in as_completed(futures):
                found.update(future.result())
    return found


def _tag_target_selection(depth: int) -> str:
    selection = "__typename oid"
    for _ in range(depth - 1):
        selection = f"__typename oid ... on Tag {{ target {{ {selection} }} }}"
    return selection


def _tag_commit_sha(target: Any) -> str | None:
    for _ in range(ACTION_TAG_RESOLUTION_DEPTH):
        if not isinstance(target, dict):
            return None
        kind = target.get("__typename")
        oid = target.get("oid")
        if kind == "Commit":
            return oid if isinstance(oid, str) and re.fullmatch(r"[0-9a-f]{40}", oid) else None
        if kind != "Tag":
            return None
        target = target.get("target")
    return None


def _resolve_tags_batch(tags: tuple[tuple[str, str], ...]) -> dict[str, str]:
    aliases = {f"a{index}": action for index, (action, _) in enumerate(tags)}
    fields: list[str] = []
    for alias, (action, tag) in zip(aliases, tags, strict=True):
        owner, name = _action_repository_parts(action)
        owner_value = json.dumps(owner)
        name_value = json.dumps(name)
        ref_name = json.dumps(f"refs/tags/{tag}")
        fields.append(
            f'{alias}: repository(owner: {owner_value}, name: {name_value}) '
            f'{{ ref(qualifiedName: {ref_name}) '
            f'{{ target {{ {_tag_target_selection(ACTION_TAG_RESOLUTION_DEPTH)} }} }} }}'
        )
    data = _graphql_batch_data(_graphql("query { " + " ".join(fields) + " }"), aliases)
    resolved: dict[str, str] = {}
    for alias, (action, tag) in zip(aliases, tags, strict=True):
        value = data.get(alias)
        ref = value.get("ref") if isinstance(value, dict) else None
        target = ref.get("target") if isinstance(ref, dict) else None
        sha = _tag_commit_sha(target)
        if sha is None:
            raise GitHubError(f"GitHub tag {action}@{tag} did not resolve to a commit")
        resolved[action] = sha
    return resolved


def resolve_tags_many(tags: dict[str, str]) -> dict[str, str]:
    items = tuple(sorted(tags.items()))
    batches = [
        items[offset : offset + GRAPHQL_BATCH_SIZE]
        for offset in range(0, len(items), GRAPHQL_BATCH_SIZE)
    ]
    found: dict[str, str] = {}
    if not batches:
        return found
    if len(batches) == 1:
        return _resolve_tags_batch(batches[0])
    with ThreadPoolExecutor(
        max_workers=min(MAX_API_WORKERS, len(batches)), thread_name_prefix="repomod-action-tags"
    ) as pool:
        for offset in range(0, len(batches), MAX_API_WORKERS):
            futures = [
                pool.submit(_resolve_tags_batch, batch)
                for batch in batches[offset : offset + MAX_API_WORKERS]
            ]
            for future in as_completed(futures):
                found.update(future.result())
    return found
