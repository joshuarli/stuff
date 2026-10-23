from __future__ import annotations

import json
import subprocess
from pathlib import Path

from repomod.types import Finding, Repository, Rule, Severity, TagPredicate

_SEVERITIES: dict[str, Severity] = {
    "error": "error",
    "warn": "warning",
    "warning": "warning",
    "note": "info",
    "info": "info",
}


def _operational(repository: Repository, message: str) -> Finding:
    return Finding(str(repository.id), "mdtool", None, message, kind="operational")


def _mdtool_findings(root: Path, repository: Repository) -> list[Finding]:
    executable = Path.home() / ".local" / "bin" / "mdtool"
    try:
        result = subprocess.run(
            [str(executable), "check", "--root", str(root), "--untracked", "--quiet", "--json"],
            cwd=root,
            capture_output=True,
            text=True,
            encoding="utf-8",
            errors="replace",
            timeout=60,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return [_operational(repository, "mdtool timed out after 60 seconds")]
    except OSError as error:
        return [_operational(repository, f"cannot run {executable}: {error}")]

    findings = []
    for line in result.stdout.splitlines():
        try:
            record = json.loads(line)
        except json.JSONDecodeError as error:
            findings.append(
                _operational(repository, f"mdtool returned invalid JSON: {error}")
            )
            continue
        if not isinstance(record, dict):
            findings.append(_operational(repository, "mdtool returned a non-object finding"))
            continue
        code = record.get("code")
        severity = record.get("severity")
        relative = record.get("file")
        message = record.get("message")
        target = record.get("target")
        line_number = record.get("line")
        column = record.get("col")
        mapped_severity = _SEVERITIES.get(severity) if isinstance(severity, str) else None
        if (
            not isinstance(code, str)
            or mapped_severity is None
            or not isinstance(relative, str)
            or not isinstance(message, str)
            or not isinstance(target, str)
        ):
            findings.append(_operational(repository, "mdtool returned a finding with invalid fields"))
            continue
        location = (
            f"line {line_number}:{column}: "
            if isinstance(line_number, int)
            and not isinstance(line_number, bool)
            and isinstance(column, int)
            and not isinstance(column, bool)
            else ""
        )
        detail = f"{location}{code} {message}"
        if target:
            detail += f": {target}"
        findings.append(
            Finding(
                str(repository.id),
                "mdtool",
                relative,
                detail,
                severity=mapped_severity,
            )
        )

    stderr = result.stderr.strip()
    has_error_finding = any(
        finding.kind == "violation" and finding.severity == "error" for finding in findings
    )
    if result.returncode not in {0, 1}:
        detail = stderr or f"exited with status {result.returncode}"
        findings.append(_operational(repository, f"mdtool failed: {detail}"))
    elif result.returncode == 1 and not has_error_finding:
        detail = stderr or "without an error finding"
        findings.append(_operational(repository, f"mdtool reported an error exit {detail}"))
    elif stderr:
        findings.append(_operational(repository, f"mdtool wrote to stderr: {result.stderr.strip()}"))
    return findings


def mdtool_rules() -> tuple[Rule, ...]:
    return (
        Rule(
            "mdtool",
            "Audit repository documentation with the local mdtool script",
            TagPredicate(),
            _mdtool_findings,
            None,
            None,
            (),
        ),
    )
