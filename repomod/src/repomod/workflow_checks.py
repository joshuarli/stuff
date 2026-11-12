from __future__ import annotations

import itertools
import re
from collections.abc import Mapping, Sequence
from pathlib import Path
from typing import Any, cast

from ruamel.yaml import YAML
from ruamel.yaml.error import YAMLError

from repomod.rules.workflows import read_workflow_sources, workflow_files

_MATRIX_REFERENCE = re.compile(r"\$\{\{\s*matrix\.([A-Za-z0-9_-]+)\s*\}\}")
_SCALAR = (str, int, float, bool, type(None))


class UndeterminableChecksError(Exception):
    pass


def produced_check_names(root: Path) -> frozenset[str] | None:
    files, problems = workflow_files(root)
    if problems or not files:
        return None
    sources, problems = read_workflow_sources(root)
    if problems:
        return None
    names: set[str] = set()
    try:
        for _, text in sources:
            names.update(_workflow_check_names(text))
    except UndeterminableChecksError:
        return None
    return frozenset(names)


def _workflow_check_names(text: str) -> set[str]:
    try:
        document = YAML(typ="safe").load(text)
    except YAMLError as error:
        raise UndeterminableChecksError from error
    if not isinstance(document, Mapping):
        raise UndeterminableChecksError
    jobs = document.get("jobs")
    if not isinstance(jobs, Mapping):
        raise UndeterminableChecksError
    names: set[str] = set()
    for job_id, job in jobs.items():
        if not isinstance(job_id, str) or not isinstance(job, Mapping):
            raise UndeterminableChecksError
        names.update(_job_check_names(job_id, job))
    return names


def _job_check_names(job_id: str, job: Mapping[str, Any]) -> list[str]:
    if "uses" in job:
        raise UndeterminableChecksError
    name = job.get("name")
    if name is not None and not isinstance(name, str):
        raise UndeterminableChecksError
    strategy = job.get("strategy")
    combinations = _matrix_combinations(strategy)
    if not combinations:
        return [_expand(name, {}) if name is not None else job_id]
    if name is None:
        return [
            f"{job_id} ({', '.join(_render(value) for value in combination.values())})"
            for combination in combinations
        ]
    return [_expand(name, combination) for combination in combinations]


def _matrix_combinations(strategy: object) -> list[dict[str, object]]:
    if strategy is None:
        return []
    if not isinstance(strategy, Mapping):
        raise UndeterminableChecksError
    matrix = strategy.get("matrix")
    if matrix is None:
        return []
    if not isinstance(matrix, Mapping):
        raise UndeterminableChecksError
    axes: dict[str, Sequence[object]] = {}
    for key, values in matrix.items():
        if key in ("include", "exclude"):
            continue
        if not isinstance(key, str) or not isinstance(values, list):
            raise UndeterminableChecksError
        axes[key] = values
    combinations: list[dict[str, object]] = (
        [dict(zip(axes, values, strict=True)) for values in itertools.product(*axes.values())]
        if axes
        else []
    )
    for entry in _entries(matrix.get("exclude")):
        combinations = [
            combination
            for combination in combinations
            if not all(combination.get(key, _MISSING) == value for key, value in entry.items())
        ]
    for entry in _entries(matrix.get("include")):
        matched = [
            combination
            for combination in combinations
            if axes and all(combination[key] == value for key, value in entry.items() if key in axes)
        ]
        if not matched:
            combinations.append(dict(entry))
            continue
        for combination in matched:
            for key, value in entry.items():
                combination.setdefault(key, value)
    return combinations


_MISSING = object()


def _entries(value: object) -> list[Mapping[str, object]]:
    if value is None:
        return []
    if not isinstance(value, list) or not all(
        isinstance(entry, Mapping) and all(isinstance(key, str) for key in entry)
        for entry in value
    ):
        raise UndeterminableChecksError
    return cast(list[Mapping[str, object]], value)


def _render(value: object) -> str:
    if not isinstance(value, _SCALAR):
        raise UndeterminableChecksError
    if value is None:
        return ""
    if isinstance(value, bool):
        return "true" if value else "false"
    text = str(value)
    if "${{" in text:
        raise UndeterminableChecksError
    return text


def _expand(template: str, combination: Mapping[str, object]) -> str:
    def substitute(match: re.Match[str]) -> str:
        key = match.group(1)
        if key not in combination:
            raise UndeterminableChecksError
        return _render(combination[key])

    expanded = _MATRIX_REFERENCE.sub(substitute, template)
    if "${{" in expanded:
        raise UndeterminableChecksError
    return expanded
