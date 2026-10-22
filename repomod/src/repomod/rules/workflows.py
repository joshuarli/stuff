from __future__ import annotations

from pathlib import Path


def workflow_files(root: Path) -> tuple[list[Path], list[tuple[str, str]]]:
    github = root / ".github"
    workflows = github / "workflows"
    if github.is_symlink():
        return [], [(".github", "workflow parent directory is a symlink")]
    if not workflows.exists() and not workflows.is_symlink():
        return [], []
    if workflows.is_symlink() or not workflows.is_dir():
        return [], [(".github/workflows", "workflow directory is not a real directory")]
    files: list[Path] = []
    problems: list[tuple[str, str]] = []
    for child in sorted(workflows.iterdir(), key=lambda item: item.name.casefold()):
        if child.suffix.casefold() not in {".yml", ".yaml"}:
            continue
        if child.is_symlink():
            problems.append((child.relative_to(root).as_posix(), "workflow symlinks are not followed"))
        elif child.is_file():
            files.append(child)
    return files, problems
