"""Tests for the ubuntu-24 fix module."""

import importlib.util
import shutil
import subprocess
from pathlib import Path

import pytest

_FIX_PATH = (
    Path(__file__).resolve().parent.parent
    / "src"
    / "repomod"
    / "fixes"
    / "ubuntu-24"
    / "main.py"
)
_spec = importlib.util.spec_from_file_location("ubuntu_24", _FIX_PATH)
assert _spec
assert _spec.loader
ubuntu_24 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ubuntu_24)


def _workflow(tmp_path: Path, content: str) -> Path:
    fp = tmp_path / ".github" / "workflows" / "ci.yml"
    fp.parent.mkdir(parents=True)
    fp.write_text(content)
    return fp


def test_upgrades_every_old_runner_label_including_arm(tmp_path: Path) -> None:
    fp = _workflow(
        tmp_path,
        "    runs-on: ubuntu-20.04\n"
        "    runs-on: ubuntu-22.04\n"
        "    runs-on: 'ubuntu-22.04-arm'\n"
        "          - os: ubuntu-22.04-arm\n",
    )
    assert ubuntu_24.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "    runs-on: ubuntu-24.04\n"
        "    runs-on: ubuntu-24.04\n"
        "    runs-on: 'ubuntu-24.04-arm'\n"
        "          - os: ubuntu-24.04-arm\n"
    )


def test_container_images_comments_and_custom_runners_are_left_alone(
    tmp_path: Path,
) -> None:
    """The old-glibc build containers are deliberate and must survive."""
    content = (
        "    container: ghcr.io/acme/test-ubuntu-20.04-amd64:0dd255f3\n"
        "    container: ubuntu:22.04\n"
        "    # Our ubuntu-20.04 glibc container defaults to an older compiler\n"
        "    runs-on: ubuntu-22.04-4core\n"
        "    runs-on: ubuntu-22.04.1\n"
        "    runs-on: ubuntu-22.04-arm64\n"
        "    runs-on: xubuntu-22.04\n"
        "    runs-on: ubuntu-latest\n"
        "    runs-on: ubuntu-24.04\n"
    )
    fp = _workflow(tmp_path, content)
    assert ubuntu_24.run(tmp_path, {}) is False
    assert fp.read_text() == content


def test_trailing_comment_survives_but_a_comment_mention_does_not_change(
    tmp_path: Path,
) -> None:
    fp = _workflow(tmp_path, "    runs-on: ubuntu-22.04 # was ubuntu-22.04 for glibc\n")
    assert ubuntu_24.run(tmp_path, {}) is True
    assert fp.read_text() == "    runs-on: ubuntu-24.04 # was ubuntu-22.04 for glibc\n"


def test_flow_matrix_collapses_old_labels_into_one_24_04_entry(
    tmp_path: Path,
) -> None:
    fp = _workflow(
        tmp_path,
        "        os: [ubuntu-20.04, ubuntu-22.04]\n"
        "        os: [ubuntu-22.04, ubuntu-24.04, macos-15]\n"
        "        os: [macos-15, ubuntu-22.04]\n"
        "        os: [ubuntu-22.04, ubuntu-22.04-arm]\n",
    )
    assert ubuntu_24.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "        os: [ubuntu-24.04]\n"
        "        os: [ubuntu-24.04, macos-15]\n"
        "        os: [macos-15, ubuntu-24.04]\n"
        "        os: [ubuntu-24.04, ubuntu-24.04-arm]\n"
    )


def test_block_matrix_collapses_old_labels_into_one_24_04_entry(
    tmp_path: Path,
) -> None:
    fp = _workflow(
        tmp_path,
        "        os:\n"
        "          - ubuntu-22.04\n"
        "          - ubuntu-20.04\n"
        "          - macos-15\n"
        "        other:\n"
        "          - ubuntu-22.04\n"
        "          - ubuntu-24.04\n",
    )
    assert ubuntu_24.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "        os:\n"
        "          - ubuntu-24.04\n"
        "          - macos-15\n"
        "        other:\n"
        "          - ubuntu-24.04\n"
    )


def test_multiline_flow_matrix_drops_the_redundant_entry_and_keeps_comments(
    tmp_path: Path,
) -> None:
    """The test matrix lists both ubuntu-24.04 and ubuntu-22.04, one item per
    line, with trailing comments."""
    fp = _workflow(
        tmp_path,
        "        os: [\n"
        "            ubuntu-24.04,\n"
        "            ubuntu-22.04,\n"
        "            ubuntu-22.04-arm,\n"
        "            macos-latest, # macOS arm64\n"
        "            windows-latest,\n"
        "          ]\n",
    )
    assert ubuntu_24.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "        os: [\n"
        "            ubuntu-24.04,\n"
        "            ubuntu-24.04-arm,\n"
        "            macos-latest, # macOS arm64\n"
        "            windows-latest,\n"
        "          ]\n"
    )


def test_multiline_flow_matrix_with_only_old_labels_collapses_to_one(
    tmp_path: Path,
) -> None:
    fp = _workflow(
        tmp_path,
        "        os: [\n"
        "            ubuntu-20.04,\n"
        "            ubuntu-22.04,\n"
        "            macos-15 ]\n",
    )
    assert ubuntu_24.run(tmp_path, {}) is True
    assert fp.read_text() == (
        "        os: [\n            ubuntu-24.04,\n            macos-15 ]\n"
    )


def test_redundant_label_on_the_closing_line_is_removed_but_the_bracket_stays(
    tmp_path: Path,
) -> None:
    fp = _workflow(
        tmp_path,
        "        os: [\n            ubuntu-24.04,\n            ubuntu-22.04 ]\n",
    )
    assert ubuntu_24.run(tmp_path, {}) is True
    assert fp.read_text() == "        os: [\n            ubuntu-24.04,\n            ]\n"


GENERATED_HEADER = (
    "# Do not edit this YAML file. This file is generated automatically by executing\n"
    "# python scripts/split_tox_gh_actions/split_tox_gh_actions.py\n"
    "# The template responsible for it is in\n"
    "# scripts/split_tox_gh_actions/templates/base.jinja\n"
)


def test_generated_workflow_files_are_left_for_their_generator(
    tmp_path: Path,
) -> None:
    """Generated test workflows are left alone; the label lives in a template."""
    generated = _workflow(tmp_path, GENERATED_HEADER + "        os: [ubuntu-22.04]\n")
    hand_written = generated.with_name("release.yml")
    hand_written.write_text("    runs-on: ubuntu-20.04\n")
    before = generated.read_text()

    assert ubuntu_24.run(tmp_path, {}) is True

    assert generated.read_text() == before
    assert hand_written.read_text() == "    runs-on: ubuntu-24.04\n"


def test_only_a_header_marks_a_file_generated_not_a_later_mention(
    tmp_path: Path,
) -> None:
    fp = _workflow(
        tmp_path,
        "name: ci\non: push\njobs:\n  build:\n    runs-on: ubuntu-22.04\n"
        "    # please do not edit the next line by hand\n",
    )

    assert ubuntu_24.run(tmp_path, {}) is True
    assert "runs-on: ubuntu-24.04" in fp.read_text()


def test_second_run_changes_nothing(tmp_path: Path) -> None:
    _workflow(tmp_path, "        os: [ubuntu-20.04, ubuntu-22.04, ubuntu-22.04-arm]\n")
    assert ubuntu_24.run(tmp_path, {}) is True
    assert ubuntu_24.run(tmp_path, {}) is False


def test_symlink_to_a_workflow_outside_the_clone_is_left_untouched(
    tmp_path: Path,
) -> None:
    repo = tmp_path / "acme" / "backend"
    outside = tmp_path / "shared-linux-runners" / "ci.yml"
    outside.parent.mkdir(parents=True)
    original = "    runs-on: ubuntu-22.04\n"
    outside.write_text(original)
    workflow = repo / ".github" / "workflows" / "ci.yml"
    workflow.parent.mkdir(parents=True)
    workflow.symlink_to(outside.resolve())

    assert ubuntu_24.run(repo, {}) is False
    assert outside.read_text() == original
    assert workflow.is_symlink()
    assert workflow.readlink().is_absolute()


def test_no_github_dir(tmp_path: Path) -> None:
    assert ubuntu_24.run(tmp_path, {}) is False


MATRIX_WORKFLOW = """\
on: push
jobs:
  test:
    runs-on: ${{ matrix.os }}
    strategy:
      matrix:
        os: [%s]
    steps:
      - run: echo hi
"""


def _actionlint_messages(root: Path) -> list[str]:
    result = subprocess.run(
        ["actionlint", ".github/workflows/ci.yml"],
        cwd=root,
        capture_output=True,
        text=True,
    )
    return [line for line in result.stdout.splitlines() if "duplicate" in line]


@pytest.mark.skipif(shutil.which("actionlint") is None, reason="needs actionlint")
def test_collapsing_is_what_keeps_actionlint_happy(tmp_path: Path) -> None:
    """A naive swap yields [ubuntu-24.04, ubuntu-24.04], which actionlint rejects."""
    fp = _workflow(tmp_path, MATRIX_WORKFLOW % "ubuntu-20.04, ubuntu-22.04")

    naive = (
        fp.read_text()
        .replace("ubuntu-20.04", "ubuntu-24.04")
        .replace("ubuntu-22.04", "ubuntu-24.04")
    )
    fp.write_text(naive)
    assert _actionlint_messages(tmp_path), "premise: naive swap must be flagged"

    fp.write_text(MATRIX_WORKFLOW % "ubuntu-20.04, ubuntu-22.04")
    assert ubuntu_24.run(tmp_path, {}) is True
    assert _actionlint_messages(tmp_path) == []
