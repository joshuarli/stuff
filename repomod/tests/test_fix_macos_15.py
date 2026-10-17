"""Tests for the macos-15 fix module."""

import importlib.util
from pathlib import Path

_FIX_PATH = (
    Path(__file__).resolve().parent.parent
    / "src"
    / "repomod"
    / "fixes"
    / "macos-15"
    / "main.py"
)
_spec = importlib.util.spec_from_file_location("macos_15", _FIX_PATH)
assert _spec
assert _spec.loader
macos_15 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(macos_15)


def _workflow(tmp_path: Path, content: str) -> Path:
    fp = tmp_path / ".github" / "workflows" / "ci.yml"
    fp.parent.mkdir(parents=True)
    fp.write_text(content)
    return fp


def test_upgrades_runs_on(tmp_path: Path) -> None:
    fp = _workflow(tmp_path, "jobs:\n  build:\n    runs-on: macos-14\n")
    assert macos_15.run(tmp_path, {}) is True
    assert fp.read_text() == "jobs:\n  build:\n    runs-on: macos-15\n"


def test_upgrades_matrix_entries_and_larger_runners(tmp_path: Path) -> None:
    fp = _workflow(
        tmp_path,
        "        os: [ubuntu-latest, macos-14]\n"
        "        runner: 'macos-14-large'\n"
        "        include:\n"
        "          - os: macos-14-xlarge\n",
    )
    assert macos_15.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "        os: [ubuntu-latest, macos-15]\n"
        "        runner: 'macos-15-large'\n"
        "        include:\n"
        "          - os: macos-15-xlarge\n"
    )


def test_leaves_other_macos_versions_alone(tmp_path: Path) -> None:
    content = (
        "runs-on: macos-13\nruns-on: macos-15\n"
        "runs-on: macos-14.5\nruns-on: xmacos-14\n"
    )
    fp = _workflow(tmp_path, content)
    assert macos_15.run(tmp_path, {}) is False
    assert fp.read_text() == content


def test_no_github_dir(tmp_path: Path) -> None:
    assert macos_15.run(tmp_path, {}) is False


def test_drops_macos_14_from_flow_matrix_that_already_has_macos_15(
    tmp_path: Path,
) -> None:
    fp = _workflow(
        tmp_path,
        "        runs-on: [macos-14, macos-15, macos-26]\n"
        "        os: [ubuntu-latest, macos-14, macos-15]\n"
        "        old: [macos-14, ubuntu-latest]\n",
    )
    assert macos_15.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "        runs-on: [macos-15, macos-26]\n"
        "        os: [ubuntu-latest, macos-15]\n"
        "        old: [macos-15, ubuntu-latest]\n"
    )


def test_drops_macos_14_from_block_matrix_that_already_has_macos_15(
    tmp_path: Path,
) -> None:
    fp = _workflow(
        tmp_path,
        "        runs-on:\n"
        "          - macos-14\n"
        "          - macos-15\n"
        "        other:\n"
        "          - macos-14\n"
        "          - ubuntu-latest\n",
    )
    assert macos_15.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "        runs-on:\n"
        "          - macos-15\n"
        "        other:\n"
        "          - macos-15\n"
        "          - ubuntu-latest\n"
    )


def test_comment_only_mentions_are_not_changed(tmp_path: Path) -> None:
    content = (
        "      # macos-14 iOS 17 not included due to flaky tests\n"
        "      runs-on: macos-15 # was macos-14\n"
    )
    fp = _workflow(tmp_path, content)
    assert macos_15.run(tmp_path, {}) is False
    assert fp.read_text() == content


def test_keeps_trailing_comment_when_upgrading(tmp_path: Path) -> None:
    fp = _workflow(tmp_path, "    runs-on: 'macos-14' # oldest simulator\n")
    assert macos_15.run(tmp_path, {}) is True
    assert fp.read_text() == "    runs-on: 'macos-15' # oldest simulator\n"


def test_second_run_changes_nothing(tmp_path: Path) -> None:
    _workflow(tmp_path, "runs-on: [macos-14, macos-15]\n")
    assert macos_15.run(tmp_path, {}) is True
    assert macos_15.run(tmp_path, {}) is False


def test_symlink_to_a_workflow_outside_the_clone_is_left_untouched(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "acme" / "mobile"
    outside = tmp_path / "shared-macos-runners" / "ci.yml"
    outside.parent.mkdir(parents=True)
    original = "    runs-on: macos-14\n"
    outside.write_text(original)
    workflow = repo / ".github" / "workflows" / "ci.yml"
    workflow.parent.mkdir(parents=True)
    workflow.symlink_to(outside.resolve())

    assert macos_15.run(repo, {}) is False
    assert outside.read_text() == original
    assert workflow.is_symlink()
    assert workflow.readlink().is_absolute()
