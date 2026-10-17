"""Tests for the pin-gha fix module."""

import importlib.util
from pathlib import Path
from unittest.mock import patch

_FIX_PATH = (
    Path(__file__).resolve().parent.parent
    / "src"
    / "repomod"
    / "fixes"
    / "pin-gha"
    / "main.py"
)
_spec = importlib.util.spec_from_file_location("pin_gha", _FIX_PATH)
assert _spec
assert _spec.loader
pin_gha = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pin_gha)


def test_accept_repo_always_true() -> None:
    repo = {"name": "acme/anything", "default_branch": "main", "topics": []}
    assert pin_gha.accept_repo(repo, {}) is True


def test_extract_repo_simple() -> None:
    assert pin_gha.extract_repo("actions/checkout") == "actions/checkout"


def test_extract_repo_with_subpath() -> None:
    assert pin_gha.extract_repo("github/codeql-action/init") == "github/codeql-action"


def test_extract_repo_deep_subpath() -> None:
    assert pin_gha.extract_repo("org/repo/a/b/c") == "org/repo"


def test_regex_plain() -> None:
    line = "      - uses: actions/checkout@v4"
    m = pin_gha.ACTION_RE.search(line)
    assert m is not None
    assert m["action"] == "actions/checkout"
    assert m["ref"] == "v4"


def test_regex_double_quoted() -> None:
    line = '      - uses: "actions/checkout@v4"'
    m = pin_gha.ACTION_RE.search(line)
    assert m is not None
    assert m["action"] == "actions/checkout"
    assert m["ref"] == "v4"


def test_regex_single_quoted() -> None:
    line = "      - uses: 'google-github-actions/setup-gcloud@v2'"
    m = pin_gha.ACTION_RE.search(line)
    assert m is not None
    assert m["action"] == "google-github-actions/setup-gcloud"
    assert m["ref"] == "v2"


def test_regex_already_pinned_with_comment() -> None:
    line = (
        "      - uses: actions/checkout@abc123def456abc123def456abc123def456abc1 # v4"
    )
    m = pin_gha.ACTION_RE.search(line)
    assert m is not None
    assert m["action"] == "actions/checkout"
    assert m["ref"] == "abc123def456abc123def456abc123def456abc1"


def test_regex_already_pinned_does_not_match_through_at() -> None:
    """The old greedy (.*) regex would match through @ on already-pinned lines."""
    line = (
        "        uses: golangci/golangci-lint-action"
        "@1e7e51e771db61008b38414a730f564565cf7c20 # pin@v8.0"
    )
    m = pin_gha.ACTION_RE.search(line)
    assert m is not None
    assert m["action"] == "golangci/golangci-lint-action"
    assert m["ref"] == "1e7e51e771db61008b38414a730f564565cf7c20"


def test_regex_subpath_action() -> None:
    line = "      - uses: github/codeql-action/init@v3"
    m = pin_gha.ACTION_RE.search(line)
    assert m is not None
    assert m["action"] == "github/codeql-action/init"
    assert m["ref"] == "v3"


def test_regex_no_match_on_non_uses_line() -> None:
    assert pin_gha.ACTION_RE.search("    runs-on: ubuntu-latest") is None
    assert pin_gha.ACTION_RE.search("    - run: echo hello") is None
    assert pin_gha.ACTION_RE.search("") is None


def test_regex_reusable_workflow() -> None:
    """Reusable workflows use org/repo/.github/workflows/file.yml@ref."""
    line = "    uses: myorg/workflows/.github/workflows/release.yml@v2"
    m = pin_gha.ACTION_RE.search(line)
    assert m is not None
    assert m["action"] == "myorg/workflows/.github/workflows/release.yml"
    assert m["ref"] == "v2"


WORKFLOW_UNPINNED = """\
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
"""

WORKFLOW_MIXED = """\
name: CI
on: push
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v4
      - uses: actions/setup-python@v5
        with:
          python-version: "3.12"
"""

FAKE_SHA = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef"


def _mock_resolve(repo: str, ref: str) -> str:
    """Return a deterministic fake SHA for testing."""
    if len(ref) == 40:
        try:
            int(ref, 16)
        except ValueError:
            pass
        else:
            return ref
    return FAKE_SHA


def test_pin_file_unpinned(tmp_path: Path) -> None:
    f = tmp_path / "ci.yml"
    f.write_text(WORKFLOW_UNPINNED)

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.pin_file(f, tmp_path)

    assert changed is True
    content = f.read_text()
    assert f"actions/checkout@{FAKE_SHA} # v4" in content
    assert f"actions/setup-python@{FAKE_SHA} # v5" in content
    # Non-uses lines should be untouched
    assert 'python-version: "3.12"' in content
    assert "name: CI" in content


def test_pin_file_already_pinned(tmp_path: Path) -> None:
    sha = "a" * 40
    f = tmp_path / "ci.yml"
    f.write_text(f"      - uses: actions/checkout@{sha} # v4\n")

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.pin_file(f, tmp_path)

    # File should not be modified
    assert changed is False
    assert f.read_text() == f"      - uses: actions/checkout@{sha} # v4\n"


def test_pin_file_mixed(tmp_path: Path) -> None:
    """One action already pinned, one not."""
    f = tmp_path / "ci.yml"
    f.write_text(WORKFLOW_MIXED)

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.pin_file(f, tmp_path)

    assert changed is True
    content = f.read_text()
    # Already-pinned line should be unchanged
    assert "actions/checkout@aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa # v4" in content
    # Unpinned line should now be pinned
    assert f"actions/setup-python@{FAKE_SHA} # v5" in content


def test_pin_file_pins_every_action_regardless_of_owner(tmp_path: Path) -> None:
    """No action owner is special-cased; the fix pins whatever it finds."""
    f = tmp_path / "ci.yml"
    f.write_text("      - uses: myorg/action-release@v1\n")

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.pin_file(f, tmp_path)

    assert changed is True
    assert f"myorg/action-release@{FAKE_SHA} # v1" in f.read_text()


def test_pin_file_strips_old_comment(tmp_path: Path) -> None:
    """When pinning, any existing trailing comment is replaced."""
    f = tmp_path / "ci.yml"
    f.write_text("      - uses: actions/checkout@v4 # old comment\n")

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.pin_file(f, tmp_path)

    assert changed is True
    content = f.read_text()
    assert f"actions/checkout@{FAKE_SHA} # v4" in content
    assert "old comment" not in content


def test_pin_file_quoted_uses(tmp_path: Path) -> None:
    f = tmp_path / "ci.yml"
    f.write_text('      - uses: "actions/checkout@v4"\n')

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.pin_file(f, tmp_path)

    assert changed is True
    assert f"actions/checkout@{FAKE_SHA} # v4" in f.read_text()


def test_pin_file_preserves_indentation(tmp_path: Path) -> None:
    """Various indentation levels should be preserved."""
    content = "    - uses: actions/checkout@v4\n        - uses: actions/setup-node@v3\n"
    f = tmp_path / "ci.yml"
    f.write_text(content)

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        pin_gha.pin_file(f, tmp_path)

    result = f.read_text()
    # First line: 4-space indent preserved
    assert result.startswith(f"    - uses: actions/checkout@{FAKE_SHA} # v4\n")
    # Second line: 8-space indent preserved
    assert f"        - uses: actions/setup-node@{FAKE_SHA} # v3\n" in result


def test_pin_file_empty_file(tmp_path: Path) -> None:
    f = tmp_path / "empty.yml"
    f.write_text("")

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        assert pin_gha.pin_file(f, tmp_path) is False

    assert f.read_text() == ""


def test_pin_file_no_uses_lines(tmp_path: Path) -> None:
    content = "name: CI\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n"
    f = tmp_path / "ci.yml"
    f.write_text(content)

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        assert pin_gha.pin_file(f, tmp_path) is False

    assert f.read_text() == content


def test_run_no_github_dir(tmp_path: Path) -> None:
    assert pin_gha.run(tmp_path, {}) is False


def test_run_finds_yml_and_yaml(tmp_path: Path) -> None:
    gh = tmp_path / ".github" / "workflows"
    gh.mkdir(parents=True)
    (gh / "ci.yml").write_text(WORKFLOW_UNPINNED)
    (gh / "release.yaml").write_text(WORKFLOW_UNPINNED)

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.run(tmp_path, {})

    assert changed is True
    assert FAKE_SHA in (gh / "ci.yml").read_text()
    assert FAKE_SHA in (gh / "release.yaml").read_text()


def test_run_finds_nested_yaml(tmp_path: Path) -> None:
    """YAML files in subdirectories of .github should also be processed."""
    nested = tmp_path / ".github" / "actions" / "setup"
    nested.mkdir(parents=True)
    (nested / "action.yml").write_text("      - uses: actions/checkout@v4\n")

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.run(tmp_path, {})

    assert changed is True
    assert FAKE_SHA in (nested / "action.yml").read_text()


def test_run_no_changes(tmp_path: Path) -> None:
    """All actions already pinned — run should return False."""
    gh = tmp_path / ".github" / "workflows"
    gh.mkdir(parents=True)
    sha = "a" * 40
    (gh / "ci.yml").write_text(f"      - uses: actions/checkout@{sha} # v4\n")

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        assert pin_gha.run(tmp_path, {}) is False


def test_symlink_to_a_workflow_outside_the_clone_is_left_untouched(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "acme" / "web"
    outside = tmp_path / "shared-actions" / "ci.yml"
    outside.parent.mkdir(parents=True)
    original = "      - uses: actions/checkout@v4\n"
    outside.write_text(original)
    workflow = repo / ".github" / "workflows" / "ci.yml"
    workflow.parent.mkdir(parents=True)
    workflow.symlink_to(outside.resolve())

    with patch.object(pin_gha, "resolve_ref", side_effect=_mock_resolve):
        changed = pin_gha.run(repo, {})

    assert changed is False
    assert outside.read_text() == original
    assert workflow.is_symlink()
    assert workflow.readlink().is_absolute()
