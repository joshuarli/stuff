from __future__ import annotations

import json
import math
import os
import re
import subprocess
import tempfile
import time
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


def remote_metadata(repositories: tuple[Repository, ...]) -> dict[str, RemoteRepository]:
    found: dict[str, RemoteRepository] = {}
    for batch in _batches(repositories):
        query, aliases = _repository_query(batch)
        try:
            data = _graphql(query).get("data", {})
        except GitHubError as error:
            for name in aliases.values():
                found[name] = RemoteRepository(name, None, None, None, str(error))
            continue
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


def discover(owner_names: tuple[str, ...]) -> list[str]:
    owners = {owner.casefold() for owner in owner_names}
    repositories: list[str] = []
    for owner_name in owner_names:
        owner_metadata = _gh_json([f"users/{owner_name}"])
        owner_type = owner_metadata.get("type") if isinstance(owner_metadata, dict) else None
        endpoint = "orgs" if owner_type == "Organization" else "users"
        pages = _gh_json(
            ["--paginate", "--slurp", f"{endpoint}/{owner_name}/repos?type=all&per_page=100"]
        )
        if not isinstance(pages, list):
            raise GitHubError(f"GitHub returned an invalid repository list for {owner_name}")
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
    for offset in range(0, len(items), GRAPHQL_BATCH_SIZE):
        checks, failures = _status_check_page(
            dict(items[offset : offset + GRAPHQL_BATCH_SIZE])
        )
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


def topics_many(
    repositories: tuple[Repository, ...], *, apply: bool, jobs: int = 4
) -> list[TopicResult]:
    if jobs < 1:
        raise ValueError("jobs must be at least one")

    def one(repo: Repository) -> TopicResult:
        try:
            if apply:
                changed, additions, removals = topics_apply(repo)
            else:
                additions, removals = topics_diff(repo)
                changed = False
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


def action_tags(repository: str, channel: str) -> list[str]:
    data = _gh_json(
        ["--paginate", "--slurp", f"repos/{repository}/git/matching-refs/tags/{channel}"]
    )
    refs: list[str] = []
    for page in data if isinstance(data, list) else []:
        if isinstance(page, list):
            for item in page:
                ref = item.get("ref") if isinstance(item, dict) else None
                if isinstance(ref, str):
                    refs.append(ref.removeprefix("refs/tags/"))
    return refs


def resolve_tag(repository: str, tag: str) -> str:
    data = _gh_json([f"repos/{repository}/git/ref/tags/{tag}"])
    if not isinstance(data, dict):
        raise GitHubError(f"GitHub did not return tag {repository}@{tag}")
    obj = data.get("object")
    for _ in range(6):
        if not isinstance(obj, dict) or not isinstance(obj.get("sha"), str):
            break
        if obj.get("type") == "commit":
            sha = obj.get("sha")
            if not isinstance(sha, str):
                break
            if re.fullmatch(r"[0-9a-f]{40}", sha) is None:
                break
            return sha
        if obj.get("type") != "tag":
            break
        obj = _gh_json([f"repos/{repository}/git/tags/{obj['sha']}"]).get("object")
    raise GitHubError(f"GitHub tag {repository}@{tag} did not resolve to a commit")
