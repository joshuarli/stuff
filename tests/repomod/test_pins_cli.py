from __future__ import annotations

import argparse
from collections.abc import Callable
from pathlib import Path

import pytest

from repomod.cli import _parser
from repomod.cmd import pins

Refresher = Callable[[Path], bool]
Validator = Callable[[Path], object]


def _args(*, check: bool = False, only: str | None = None) -> argparse.Namespace:
    return argparse.Namespace(operation="refresh", check=check, only=only)


def _install(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    files: dict[str, str],
    refreshers: dict[str, Refresher],
    validators: dict[str, Validator] | None = None,
) -> dict[str, Path]:
    targets = {}
    for name, content in files.items():
        path = tmp_path / f"{name}.toml"
        path.write_text(content, encoding="utf-8")
        targets[name] = path
    scratch = tmp_path / "scratch"
    scratch.mkdir()

    monkeypatch.setattr(pins, "_targets", lambda: targets)
    monkeypatch.setattr(pins, "_refreshers", lambda: refreshers)
    monkeypatch.setattr(
        pins,
        "_validators",
        lambda: validators if validators is not None else {name: (lambda path: None) for name in files},
    )
    monkeypatch.setattr(pins, "_scratch_directory", lambda: scratch)
    monkeypatch.setattr(pins, "_musl_plan", lambda path: pins._Plan(path, None))
    return targets


def test_check_reports_drift_without_writing(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def change(path: Path) -> bool:
        path.write_text("new\n", encoding="utf-8")
        return True

    targets = _install(tmp_path, monkeypatch, {"actions": "old\n"}, {"actions": change})

    assert pins._pins(_args(check=True, only="actions")) == 1

    captured = capsys.readouterr()
    assert f"{targets['actions']}: would change" in captured.out
    assert targets["actions"].read_text(encoding="utf-8") == "old\n"


def test_check_is_clean_when_nothing_would_change(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    _install(tmp_path, monkeypatch, {"actions": "old\n"}, {"actions": lambda path: False})

    assert pins._pins(_args(check=True, only="actions")) == 0
    assert capsys.readouterr().out == ""


def test_refresh_writes_a_changed_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def change(path: Path) -> bool:
        path.write_text("new\n", encoding="utf-8")
        return True

    targets = _install(tmp_path, monkeypatch, {"actions": "old\n"}, {"actions": change})

    assert pins._pins(_args(only="actions")) == 0
    assert targets["actions"].read_text(encoding="utf-8") == "new\n"


def test_refresh_leaves_an_unchanged_file_alone(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    targets = _install(
        tmp_path, monkeypatch, {"python": "same\n"}, {"python": lambda path: False}
    )
    before = targets["python"].stat()

    assert pins._pins(_args(only="python")) == 0

    after = targets["python"].stat()
    assert (after.st_ino, after.st_mtime_ns) == (before.st_ino, before.st_mtime_ns)


def test_a_failed_domain_writes_no_file(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    def change(path: Path) -> bool:
        path.write_text("new\n", encoding="utf-8")
        return True

    def fail(path: Path) -> bool:
        raise ValueError("offline")

    targets = _install(
        tmp_path,
        monkeypatch,
        {"actions": "old\n", "python": "old\n", "typescript": "old\n", "musl": "old\n"},
        {
            "actions": change,
            "python": fail,
            "typescript": lambda path: False,
        },
    )

    assert pins._pins(_args()) == 2

    for name in ("actions", "python", "typescript", "musl"):
        assert targets[name].read_text(encoding="utf-8") == "old\n"
    assert "cannot refresh python pins: offline" in capsys.readouterr().err


def test_only_limits_the_refresh_to_one_domain(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    def unexpected(path: Path) -> bool:
        raise AssertionError("actions must not be refreshed")

    targets = _install(
        tmp_path,
        monkeypatch,
        {"actions": "old\n", "python": "old\n"},
        {"actions": unexpected, "python": lambda path: False},
    )

    assert pins._pins(_args(only="python")) == 0
    assert targets["actions"].read_text(encoding="utf-8") == "old\n"


def test_symlinked_target_is_rejected(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    real = tmp_path / "real.toml"
    real.write_text("old\n", encoding="utf-8")
    link = tmp_path / "actions.toml"
    link.symlink_to(real)
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    monkeypatch.setattr(pins, "_targets", lambda: {"actions": link})
    monkeypatch.setattr(pins, "_refreshers", lambda: {"actions": lambda path: False})
    monkeypatch.setattr(pins, "_validators", lambda: {"actions": lambda path: None})
    monkeypatch.setattr(pins, "_scratch_directory", lambda: scratch)

    assert pins._pins(_args(only="actions")) == 2
    assert real.read_text(encoding="utf-8") == "old\n"


def test_musl_observations_are_printed(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    target = tmp_path / "musl-toolchain.toml"
    target.write_text("old\n", encoding="utf-8")
    scratch = tmp_path / "scratch"
    scratch.mkdir()
    monkeypatch.setattr(pins, "_targets", lambda: {"musl": target})
    monkeypatch.setattr(pins, "_scratch_directory", lambda: scratch)
    monkeypatch.setattr(
        pins,
        "_musl_plan",
        lambda path: pins._Plan(path, None, ("alpine:3.24 currently releases Alpine 3.24.3",)),
    )

    assert pins._pins(_args(check=True, only="musl")) == 0
    assert "currently releases Alpine 3.24.3" in capsys.readouterr().err


def test_parser_accepts_pins_refresh_options() -> None:
    args = _parser().parse_args(["pins", "refresh", "--check", "--only", "musl"])

    assert args.command == "pins"
    assert args.operation == "refresh"
    assert args.check is True
    assert args.only == "musl"


def test_parser_rejects_an_unknown_pin_domain() -> None:
    with pytest.raises(SystemExit):
        _parser().parse_args(["pins", "refresh", "--only", "rust"])
