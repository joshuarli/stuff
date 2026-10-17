"""Harness for running the real --apply pipeline against a fake `gh` and local git.

`gh` is a stub script on PATH that records every invocation (with a timestamp)
and answers from canned behavior, so tests can assert exactly which GitHub API
calls a run makes. Git is real: each repo's `origin` is a local bare repo, so
pushes land somewhere a test can inspect.
"""

import json
import shutil
import subprocess
import tempfile
from pathlib import Path
from typing import Any
from unittest.mock import patch

GH_SCRIPT = """\
#!__PYTHON__
import json, re, sys, time
from pathlib import Path

root = Path(__file__).parent
args = sys.argv[1:]
with open(root / "calls.jsonl", "a") as f:
    f.write(json.dumps({"args": args, "time": time.time()}) + "\\n")
cfg = json.loads((root / "behavior.json").read_text())


def opt(name):
    return args[args.index(name) + 1]


if args[:2] == ["api", "graphql"] and "resource(url" in " ".join(args):
    query = next(a for a in args if a.startswith("query="))[len("query="):]
    data = {}
    for alias, url in re.findall(r'(r\\d+): resource\\(url: "([^"]+)"', query):
        m = re.fullmatch(r"https://github.com/(.+)/pull/(\\d+)", url)
        repo = m.group(1).replace("/", "_")
        data[alias] = {"id": f"PR_{repo}_{m.group(2)}"}
    print(json.dumps({"data": data}))
elif args[:2] == ["api", "graphql"] and "nodes(ids" in " ".join(args):
    query = next(a for a in args if a.startswith("query="))[len("query="):]
    ids = json.loads(re.search(r"nodes\\(ids: (\\[.*?\\])\\)", query).group(1))
    ready_file = root / "ready.json"
    ready = json.loads(ready_file.read_text()) if ready_file.exists() else []
    items = {i["node_id"]: i for i in cfg["search_items"]}
    nodes = []
    for node_id in ids:
        item = items.get(node_id)
        if item is None:
            nodes.append(None)
            continue
        merged = item["pull_request"].get("merged_at")
        nodes.append({
            "id": node_id,
            "url": item["html_url"],
            "state": "MERGED" if merged else item["state"].upper(),
            "isDraft": bool(item.get("draft")) and node_id not in ready,
            "repository": {"nameWithOwner": item["repository_url"].split("/repos/")[1]},
        })
    print(json.dumps({"data": {"nodes": nodes}}))
elif args[:2] == ["api", "graphql"]:
    query = next(a for a in args if a.startswith("query="))[len("query="):]
    marks = re.findall(
        r'(m\\d+): markPullRequestReadyForReview\\(input: \\{pullRequestId: "([^"]+)"',
        query,
    )
    urls = {i["node_id"]: i["html_url"] for i in cfg["search_items"]}
    data, errors = {}, []
    for alias, node_id in marks:
        if node_id in cfg["graphql_fail"]:
            data[alias] = None
            errors.append({"message": "Could not resolve to a node", "path": [alias]})
        else:
            url = urls.get(node_id, "")
            data[alias] = {"pullRequest": {"isDraft": False, "url": url}}
            ready_file = root / "ready.json"
            ready = json.loads(ready_file.read_text()) if ready_file.exists() else []
            ready_file.write_text(json.dumps(ready + [node_id]))
    print(json.dumps({"data": data, **({"errors": errors} if errors else {})}))
    sys.exit(1 if errors else 0)
elif args[:1] == ["api"]:
    for item in cfg["search_items"]:
        print(json.dumps(item))
elif args[:2] == ["pr", "create"]:
    repo = opt("--repo")
    if repo in cfg["already_exists"]:
        print("a pull request for branch already exists", file=sys.stderr)
        sys.exit(1)
    if cfg.get("create_prints_url", True):
        print(f"https://github.com/{repo}/pull/1")
elif args[:2] == ["pr", "view"]:
    repo = opt("--repo")
    if repo not in cfg["views"]:
        sys.exit(1)
    print(json.dumps(cfg["views"][repo]))
"""


class FakeGh:
    def __init__(self, root: Path) -> None:
        self.root = root
        self.configure()

    def configure(
        self,
        search_items: list[dict[str, Any]] | None = None,
        already_exists: list[str] | None = None,
        views: dict[str, dict[str, str]] | None = None,
        create_prints_url: bool = True,
        graphql_fail: list[str] | None = None,
    ) -> None:
        (self.root / "behavior.json").write_text(
            json.dumps(
                {
                    "search_items": search_items or [],
                    "already_exists": already_exists or [],
                    "views": views or {},
                    "create_prints_url": create_prints_url,
                    "graphql_fail": graphql_fail or [],
                }
            )
        )

    def calls(self) -> list[dict[str, Any]]:
        log = self.root / "calls.jsonl"
        if not log.exists():
            return []
        return [json.loads(line) for line in log.read_text().splitlines()]

    def count(self, *prefix: str, repo: str | None = None) -> int:
        """Calls whose args start with `prefix`, optionally for one `--repo`."""
        n = 0
        for call in self.calls():
            args = call["args"]
            if args[: len(prefix)] != list(prefix):
                continue
            if repo is not None and (
                "--repo" not in args or args[args.index("--repo") + 1] != repo
            ):
                continue
            n += 1
        return n

    def graphql(self, marker: str) -> list[str]:
        """Queries of GraphQL calls whose text contains `marker`."""
        queries = []
        for call in self.calls():
            args = call["args"]
            if args[:2] == ["api", "graphql"]:
                query = str(next(a for a in args if a.startswith("query=")))
                if marker in query:
                    queries.append(query)
        return queries

    def calls_for(self, repo: str) -> list[list[str]]:
        return [
            c["args"]
            for c in self.calls()
            if "--repo" in c["args"]
            and c["args"][c["args"].index("--repo") + 1] == repo
        ]


def node_id(repo: str, number: int) -> str:
    return f"PR_{repo.replace('/', '_')}_{number}"


def search_item(
    repo: str,
    number: int,
    state: str,
    merged_at: str | None = None,
    draft: bool = False,
) -> dict[str, Any]:
    """One GitHub search/issues result for a PR."""
    return {
        "node_id": node_id(repo, number),
        "html_url": f"https://github.com/{repo}/pull/{number}",
        "repository_url": f"https://api.github.com/repos/{repo}",
        "state": state,
        "draft": draft,
        "pull_request": {"merged_at": merged_at},
    }


def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True
    ).stdout


_templates: tuple[tempfile.TemporaryDirectory[str], Path, Path] | None = None


def _repo_template() -> tuple[Path, Path]:
    """A bare origin and a working clone, built once and copied per test.

    Building a repo from scratch costs nine git processes; copying a prebuilt
    pair and rewriting its origin cuts that to one, and every --apply test needs
    three repos. The TemporaryDirectory is held for the process so it is cleaned
    up at exit rather than the moment this function returns.
    """
    global _templates
    if _templates is None:
        tmp = tempfile.TemporaryDirectory(prefix="repomod-repo-template-")
        root = Path(tmp.name)
        bare = root / "origin.git"
        seed = root / "seed"
        clone = root / "clone"
        for d in (bare, seed):
            d.mkdir(parents=True)
        _git(bare, "init", "--bare", "--initial-branch=main")
        _git(seed, "init", "--initial-branch=main")
        (seed / "README.md").write_text("hello")
        _git(seed, "add", ".")
        _git(
            seed,
            "-c",
            "user.email=t@t.com",
            "-c",
            "user.name=T",
            "commit",
            "-m",
            "init",
        )
        _git(seed, "push", str(bare), "main")
        _git(root, "clone", str(bare), str(clone))
        _git(clone, "config", "user.email", "test@test.com")
        _git(clone, "config", "user.name", "Test")
        _templates = (tmp, bare, clone)
    return _templates[1], _templates[2]


def make_repo_with_origin(base: Path, name: str) -> tuple[Path, Path]:
    """A working clone whose `origin` is a local bare repo. Returns (clone, bare)."""
    template_bare, template_clone = _repo_template()
    bare = base / "origins" / f"{name}.git"
    clone = base / "clones" / name
    bare.parent.mkdir(parents=True, exist_ok=True)
    clone.parent.mkdir(parents=True, exist_ok=True)
    shutil.copytree(template_bare, bare)
    shutil.copytree(template_clone, clone)
    _git(clone, "remote", "set-url", "origin", str(bare))
    return clone, bare


def branch_pushed(bare: Path, branch: str) -> bool:
    return bool(_git(bare, "branch", "--list", branch).strip())


def offline_clone() -> Any:
    """Make repomod's clone() reuse an existing local clone without fetching."""
    original_run = subprocess.run

    def side_effect(cmd: Any, **kwargs: Any) -> subprocess.CompletedProcess[Any]:
        cmd_str = " ".join(str(c) for c in cmd)
        if any(
            x in cmd_str
            for x in (
                "git fetch",
                "git reset --hard",
                "git checkout --force",
                "git branch --list --format",
            )
        ):
            return subprocess.CompletedProcess(cmd, 0, stdout="main\n", stderr="")
        return original_run(cmd, **kwargs)

    return patch("repomod.git.subprocess.run", side_effect=side_effect)
