"""Tests for FixConfig parsing."""

from pathlib import Path
from typing import Any

import pytest

from repomod.types import FixConfig

_BASE = (
    "branch: pin-gha-actions\n"
    'pr_title: "chore: pin actions"\n'
    "pr_body: |\n"
    "  Pin actions.\n"
)


def _write_config(tmp_path: Path, extra: str) -> Path:
    path = tmp_path / "config.yaml"
    path.write_text(_BASE + extra)
    return path


def test_scalar_skip_is_rejected(tmp_path: Path) -> None:
    path = _write_config(tmp_path, "skip: acme/backend\n")
    with pytest.raises(ValueError, match="skip must be a list of strings"):
        FixConfig.from_yaml(path)


def test_scalar_reviewers_is_rejected(tmp_path: Path) -> None:
    path = _write_config(tmp_path, "reviewers: acme/backend\n")
    with pytest.raises(ValueError, match="reviewers must be a list of strings"):
        FixConfig.from_yaml(path)


def test_scalar_sparse_paths_is_rejected(tmp_path: Path) -> None:
    path = _write_config(tmp_path, "sparse_paths: acme/backend\n")
    with pytest.raises(ValueError, match="sparse_paths must be a list of strings"):
        FixConfig.from_yaml(path)


@pytest.mark.parametrize(
    ("key", "body"),
    [
        ("skip", "skip:\n  acme/backend: true\n"),
        ("reviewers", "reviewers:\n  acme/backend: true\n"),
        ("sparse_paths", "sparse_paths:\n  acme/backend: true\n"),
        ("skip", "skip:\n  - acme/backend\n  - 1\n"),
        ("reviewers", "reviewers:\n  - acme/backend\n  - 1\n"),
        ("sparse_paths", "sparse_paths:\n  - acme/backend\n  - 1\n"),
        ("skip", "skip: false\n"),
        ("reviewers", "reviewers: false\n"),
        ("sparse_paths", "sparse_paths: false\n"),
        ("skip", 'skip: ""\n'),
        ("reviewers", 'reviewers: ""\n'),
        ("sparse_paths", 'sparse_paths: ""\n'),
    ],
    ids=(
        "skip-mapping",
        "reviewers-mapping",
        "sparse_paths-mapping",
        "skip-non-string",
        "reviewers-non-string",
        "sparse_paths-non-string",
        "skip-false",
        "reviewers-false",
        "sparse_paths-false",
        "skip-empty-string",
        "reviewers-empty-string",
        "sparse_paths-empty-string",
    ),
)
def test_string_list_fields_reject_other_shapes(
    tmp_path: Path, key: str, body: str
) -> None:
    path = _write_config(tmp_path, body)
    with pytest.raises(ValueError, match=f"{key} must be a list of strings"):
        FixConfig.from_yaml(path)


def test_skip_list_and_null_sparse_paths_load(tmp_path: Path) -> None:
    path = _write_config(
        tmp_path,
        "reviewers:\n"
        "  - acme/backend\n"
        "skip:\n"
        "  - acme/backend\n"
        "sparse_paths: null\n",
    )
    config = FixConfig.from_yaml(path)
    assert config.reviewers == ["acme/backend"]
    assert config.skip == ["acme/backend"]
    assert "acme/backend" in set(config.skip)
    assert config.sparse_paths is None


def test_omitted_skip_reviewers_and_sparse_paths_use_defaults(tmp_path: Path) -> None:
    config = FixConfig.from_yaml(_write_config(tmp_path, ""))
    assert config.skip == []
    assert config.reviewers == []
    assert config.sparse_paths is None


def test_null_skip_and_reviewers_are_empty_lists(tmp_path: Path) -> None:
    config = FixConfig.from_yaml(
        _write_config(tmp_path, "skip: null\nreviewers: null\nsparse_paths:\n")
    )
    assert config.skip == []
    assert config.reviewers == []
    assert config.sparse_paths is None


def test_empty_string_lists_stay_empty(tmp_path: Path) -> None:
    config = FixConfig.from_yaml(
        _write_config(tmp_path, "skip: []\nreviewers: []\nsparse_paths: []\n")
    )
    assert config.skip == []
    assert config.reviewers == []
    assert config.sparse_paths == []


def test_scalar_skip_cannot_be_constructed() -> None:
    skip: Any = "acme/backend"
    with pytest.raises(ValueError, match="skip must be a list of strings"):
        FixConfig(branch="b", pr_title="t", pr_body="p", skip=skip)
