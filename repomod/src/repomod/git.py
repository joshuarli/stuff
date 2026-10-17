"""Git and GitHub operations: clone, diff, push, PR creation."""

import json
import shutil
import subprocess
from collections.abc import Generator
from contextlib import contextmanager
from pathlib import Path
from typing import Any

from repomod.types import OWNER_QUALIFIERS, PRInfo, PRSpec, RolloutPR
from repomod.utils import (
    SUBPROCESS_TIMEOUT,
    ShutdownError,
    github_rate_limiter,
    interrupt,
    pr_create_limiter,
    retry,
)


def _run(args: list[str], **kwargs: Any) -> subprocess.CompletedProcess[Any]:
    """Run git or gh outside the foreground process group.

    SIGINT reaches every process in that group, so a child left there dies
    with CalledProcessError(-2) while this process has only set its interrupt
    event. A new session lets the command finish. Success still returns when
    ^C is already set, so a dry run can read the diff it just produced. A
    child error then is cancellation, not the child's failure.
    """
    kwargs.setdefault("timeout", SUBPROCESS_TIMEOUT)
    kwargs["start_new_session"] = True
    try:
        return subprocess.run(args, **kwargs)
    except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as exc:
        if interrupt.is_set:
            raise ShutdownError from exc
        raise


def git_run(
    args: list[str], cwd: Path, **kwargs: Any
) -> subprocess.CompletedProcess[Any]:
    """Run a git command with timeout, capturing output and raising on failure."""
    return _run(
        ["git", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
        **kwargs,
    )


def gh_run(args: list[str], **kwargs: Any) -> subprocess.CompletedProcess[Any]:
    """Run a gh command with rate limiting and timeout."""
    kwargs.setdefault("capture_output", True)
    kwargs.setdefault("text", True)
    with github_rate_limiter:
        result = _run(["gh", *args], **kwargs)
    if result.returncode != 0:
        interrupt.check()
    return result


@retry()
def clone(
    name: str, default_branch: str, dest: Path, sparse_paths: list[str] | None
) -> None:
    """Shallow-clone a repo, or reset an existing clone to its default branch."""
    if dest.exists() and not (dest / ".git").exists():
        shutil.rmtree(dest)

    if dest.exists():
        interrupt.check()
        git_run(["checkout", "--force", default_branch], cwd=dest)
        interrupt.check()
        with github_rate_limiter:
            git_run(["fetch", "--depth=1", "origin", default_branch], cwd=dest)
        interrupt.check()
        git_run(["reset", "--hard", f"origin/{default_branch}"], cwd=dest)
        interrupt.check()
        result = git_run(
            ["branch", "--list", "--format=%(refname:short)"], cwd=dest, text=True
        )
        for branch in result.stdout.splitlines():
            branch = branch.strip()
            if branch and branch != default_branch:
                interrupt.check()
                _run(
                    ["git", "branch", "-D", branch],
                    cwd=dest,
                    capture_output=True,
                )
        return

    interrupt.check()
    cmd = ["git", "clone", "--depth=1", "--filter=blob:none", "--single-branch"]
    if sparse_paths is not None:
        cmd.append("--sparse")
    cmd += [f"https://github.com/{name}.git", str(dest)]
    with github_rate_limiter:
        _run(cmd, check=True, capture_output=True)

    if sparse_paths is not None:
        interrupt.check()
        git_run(["sparse-checkout", "set", *sparse_paths], cwd=dest)


def has_changes(repo_dir: Path) -> bool:
    return _run(["git", "diff", "--quiet"], cwd=repo_dir).returncode != 0


def changed_files(repo_dir: Path) -> list[str]:
    """Repo-relative paths of tracked files modified in the working tree."""
    result = git_run(["diff", "--name-only"], cwd=repo_dir, text=True)
    return [line for line in str(result.stdout).splitlines() if line]


@contextmanager
def stashed_changes(repo_dir: Path) -> Generator[None]:
    """Temporarily revert the working tree to HEAD, restoring changes on exit."""
    git_run(["stash"], cwd=repo_dir)
    try:
        yield
    finally:
        git_run(["stash", "pop"], cwd=repo_dir)


def head_sha(repo_dir: Path) -> str:
    """Commit the working tree is based on, so a patch can be matched to its base."""
    result = git_run(["rev-parse", "HEAD"], cwd=repo_dir, text=True)
    return str(result.stdout).strip()


def apply_patch(repo_dir: Path, patch: str) -> bool:
    """Apply a stored diff to the working tree. False if it doesn't apply cleanly.

    git apply is all-or-nothing, so on failure the tree is left untouched.
    """
    try:
        git_run(
            ["apply", "--whitespace=nowarn", "-"], cwd=repo_dir, input=patch.encode()
        )
    except subprocess.CalledProcessError:
        return False
    return True


def get_diff(repo_dir: Path) -> str:
    stdout = _run(
        ["git", "diff"],
        cwd=repo_dir,
        capture_output=True,
        text=True,
    ).stdout
    assert isinstance(stdout, str)
    return stdout


def commit_and_push(
    repo_dir: Path, pr: PRSpec, known_pr: RolloutPR | None = None
) -> PRInfo:
    """Commit, push, and open a PR.

    `known_pr` is the repo's existing PR on this branch, looked up once for the
    whole run (see search_prs) rather than per repo to save API calls.

    Each step is idempotent so partial failures are safe to re-run. If the
    branch's PR was already merged or closed, nothing is pushed and that PR is
    returned: force-pushing would silently resurrect a branch whose PR someone
    finished or deliberately rejected. Roll out a new generation by changing
    the branch name in config.yaml.
    """
    if known_pr and known_pr.state != "OPEN":
        return PRInfo(known_pr.url, known_pr.state, known_pr.node_id)

    interrupt.check()
    result = git_run(["branch", "--show-current"], cwd=repo_dir, text=True)
    if result.stdout.strip() != pr.branch:
        interrupt.check()
        existing = git_run(["branch", "--list", pr.branch], cwd=repo_dir, text=True)
        interrupt.check()
        if existing.stdout.strip():
            git_run(["checkout", pr.branch], cwd=repo_dir)
        else:
            git_run(["checkout", "-b", pr.branch], cwd=repo_dir)

    interrupt.check()
    git_run(["add", "--all"], cwd=repo_dir)

    interrupt.check()
    if _run(["git", "diff", "--cached", "--quiet"], cwd=repo_dir).returncode != 0:
        interrupt.check()
        git_run(["commit", "--quiet", "--message", pr.title], cwd=repo_dir)

    interrupt.check()
    return push_and_open_pr(repo_dir, pr, known_pr)


def find_pr(name: str, branch: str) -> PRInfo | None:
    """The PR for a head branch (open if there is one, else the latest), if any."""
    result = gh_run(["pr", "view", branch, "--repo", name, "--json", "url,state"])
    interrupt.check()
    if result.returncode != 0:
        return None
    try:
        data = json.loads(result.stdout)
        return PRInfo(url=data["url"], state=data["state"])
    except json.JSONDecodeError, KeyError, TypeError:
        return None


@retry()
def push_and_open_pr(repo_dir: Path, pr: PRSpec, known_pr: RolloutPR | None) -> PRInfo:
    """Push and ensure a PR exists. Retried on transient failures."""
    interrupt.check()
    with github_rate_limiter:
        git_run(
            ["push", "--set-upstream", "origin", pr.branch, "--force", "--quiet"],
            cwd=repo_dir,
        )

    interrupt.check()
    # The push already updated an open PR; no API call needed.
    if known_pr:
        return PRInfo(known_pr.url, "OPEN", known_pr.node_id)

    # Reviewers go on the create call: a separate `pr edit` per reviewer would
    # cost an API call each, and re-requesting on later runs would re-ping people.
    reviewer_args = [arg for r in pr.reviewers for arg in ("--reviewer", r)]
    label_args = [arg for label in pr.labels for arg in ("--label", label)]
    interrupt.check()
    with pr_create_limiter:
        result = gh_run(
            [
                "pr",
                "create",
                "--repo",
                pr.repo_name,
                "--head",
                pr.branch,
                # Mass-created PRs open as drafts so nothing notifies reviewers or
                # runs ready-only checks until the rollout is reviewed; `--undraft`
                # promotes them.
                "--draft",
                "--title",
                pr.title,
                "--body",
                pr.body,
                *reviewer_args,
                *label_args,
            ]
        )
    interrupt.check()
    if result.returncode != 0:
        # The search that produced known_pr can lag behind a PR created moments
        # ago (e.g. by an interrupted earlier run); recover it instead of failing.
        if "already exists" in result.stderr:
            found = find_pr(pr.repo_name, pr.branch)
            if found:
                return found
        raise subprocess.CalledProcessError(
            result.returncode, result.args, result.stdout, result.stderr
        )

    # On success `gh pr create` prints the new PR's URL, so there's no need to
    # spend another call looking it up.
    url = next(
        (
            line
            for line in reversed(result.stdout.splitlines())
            if line.startswith("https://")
        ),
        None,
    )
    if url is None:
        found = find_pr(pr.repo_name, pr.branch)
        if found:
            return found
        raise subprocess.CalledProcessError(
            result.returncode,
            result.args,
            result.stdout,
            "gh pr create printed no PR URL",
        )
    return PRInfo(url, "OPEN")


RESOLVE_BATCH_SIZE = 50


@retry()
def _resolve_batch(urls: list[str]) -> dict[str, str]:
    aliases = " ".join(
        f"r{i}: resource(url: {json.dumps(url)}) {{ ... on PullRequest {{ id }} }}"
        for i, url in enumerate(urls)
    )
    result = gh_run(["api", "graphql", "-f", f"query=query {{ {aliases} }}"])
    try:
        data = json.loads(result.stdout)["data"]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        raise subprocess.CalledProcessError(
            result.returncode, result.args, result.stdout, result.stderr
        ) from exc
    return {url: data[f"r{i}"]["id"] for i, url in enumerate(urls) if data.get(f"r{i}")}


def resolve_pr_node_ids(urls: list[str]) -> dict[str, str]:
    """GraphQL node ids for PR URLs, batched RESOLVE_BATCH_SIZE per API call.

    Ids never change, so callers cache them. URLs that don't resolve are omitted.
    """
    ids: dict[str, str] = {}
    for start in range(0, len(urls), RESOLVE_BATCH_SIZE):
        ids.update(_resolve_batch(urls[start : start + RESOLVE_BATCH_SIZE]))
    return ids


STATE_BATCH_SIZE = 100
READY_BATCH_SIZE = 20


@retry()
def _state_batch(node_ids: list[str]) -> list[RolloutPR]:
    result = gh_run(
        [
            "api",
            "graphql",
            "-f",
            "query=query { nodes(ids: "
            + json.dumps(node_ids)
            + ") { ... on PullRequest { id url state isDraft"
            " repository { nameWithOwner } } } }",
        ]
    )
    try:
        nodes = json.loads(result.stdout)["data"]["nodes"]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        raise subprocess.CalledProcessError(
            result.returncode, result.args, result.stdout, result.stderr
        ) from exc
    return [
        RolloutPR(
            repo=node["repository"]["nameWithOwner"],
            url=node["url"],
            state=node["state"],
            draft=node["isDraft"],
            node_id=node["id"],
        )
        for node in nodes
        if node
    ]


def pr_states(node_ids: list[str]) -> list[RolloutPR]:
    """Live state of PRs by node id, STATE_BATCH_SIZE per API call.

    Ids can be cached forever but draft/merged state cannot, so this is the one
    cheap call that makes cached ids useful.
    """
    prs: list[RolloutPR] = []
    for start in range(0, len(node_ids), STATE_BATCH_SIZE):
        prs += _state_batch(node_ids[start : start + STATE_BATCH_SIZE])
    return prs


@retry()
def _ready_batch(prs: list[RolloutPR]) -> list[RolloutPR]:
    """Mark PRs ready for review in one request. Returns the ones that failed."""
    mutations = " ".join(
        f"m{i}: markPullRequestReadyForReview(input: "
        f"{{pullRequestId: {json.dumps(pr.node_id)}}}) {{ pullRequest {{ isDraft }} }}"
        for i, pr in enumerate(prs)
    )
    # Marking ready notifies reviewers and requests code owners, so it gets the
    # same slow spacing as creating PRs.
    with pr_create_limiter:
        result = gh_run(["api", "graphql", "-f", f"query=mutation {{ {mutations} }}"])
    # GraphQL reports partial failure as null aliases plus an errors list (and gh
    # exits non-zero), so judge by the payload, not the exit code.
    try:
        data = json.loads(result.stdout)["data"]
    except (json.JSONDecodeError, KeyError, TypeError) as exc:
        raise subprocess.CalledProcessError(
            result.returncode, result.args, result.stdout, result.stderr
        ) from exc
    return [pr for i, pr in enumerate(prs) if not data.get(f"m{i}")]


def mark_ready(prs: list[RolloutPR]) -> list[RolloutPR]:
    """Mark draft PRs ready for review, READY_BATCH_SIZE per API call.

    Returns the PRs GitHub refused.
    """
    failed: list[RolloutPR] = []
    for start in range(0, len(prs), READY_BATCH_SIZE):
        failed += _ready_batch(prs[start : start + READY_BATCH_SIZE])
    return failed


@retry()
def search_prs(branch: str) -> list[RolloutPR]:
    """All PRs owned by the configured owners whose head branch is `branch`.

    Asks GitHub rather than reading status.yaml, which only knows about PRs
    this checkout created. Search returns at most 1000 results.
    """
    owners = " ".join(OWNER_QUALIFIERS)
    result = gh_run(
        [
            "api",
            "--method",
            "GET",
            "search/issues",
            "-f",
            f"q=is:pr {owners} head:{branch}",
            "-f",
            "per_page=100",
            "--paginate",
            "--jq",
            ".items[]",
        ],
        check=True,
    )
    prs: list[RolloutPR] = []
    for line in result.stdout.splitlines():
        item = json.loads(line)
        if item["pull_request"].get("merged_at"):
            state = "MERGED"
        else:
            state = item["state"].upper()
        prs.append(
            RolloutPR(
                repo=item["repository_url"].split("/repos/", 1)[1],
                url=item["html_url"],
                state=state,
                draft=bool(item.get("draft")),
                node_id=item["node_id"],
            )
        )
    return sorted(prs)


def pr_by_repo(prs: list[RolloutPR]) -> dict[str, RolloutPR]:
    """One PR per repo, preferring an open one: a branch can have an old closed
    PR alongside a current open one."""
    index: dict[str, RolloutPR] = {}
    for pr in prs:
        if pr.repo not in index or pr.state == "OPEN":
            index[pr.repo] = pr
    return index


def format_error(exc: subprocess.CalledProcessError | subprocess.TimeoutExpired) -> str:
    """Format a subprocess error with stderr for readable diagnostics."""
    if isinstance(exc, subprocess.TimeoutExpired):
        return f"Command {exc.cmd!r} timed out after {exc.timeout}s"
    parts = [f"Command {exc.cmd!r} returned exit code {exc.returncode}"]
    if exc.stderr:
        parts.append(f"stderr: {exc.stderr.strip()}")
    if exc.stdout:
        parts.append(f"stdout: {exc.stdout.strip()}")
    return "\n".join(parts)
