from __future__ import annotations

import argparse

from repomod import policy

from . import common


def _lint(args: argparse.Namespace) -> int:
    catalog = common._catalog()
    repositories = common._select(catalog, args.repo, args.tag)
    findings = policy.lint_repositories(
        catalog,
        repositories,
        tags=common._tags(args.tag),
        rule_ids=args.rule,
        checkouts=common._checkouts(getattr(args, "checkout", None), args.repo, repositories),
    )
    if args.json:
        common._print_json([policy.finding_dict(finding) for finding in findings])
    else:
        current: str | None = None
        for finding in findings:
            if finding.repository != current:
                if current is not None:
                    print()
                print(common._paint(finding.repository, "36"))
                current = finding.repository
            print(f"    {common._paint(policy.format_finding_short(finding), common._finding_color(finding))}")
        for code, description in policy.lint_legend(findings):
            print(f"{code}: {description}")
        if not findings:
            print("no policy findings")
    if any(finding.kind == "operational" for finding in findings):
        return 2
    has_errors = any(
        finding.kind == "violation" and finding.severity == "error"
        for finding in findings
    )
    has_warnings = any(finding.severity == "warning" for finding in findings)
    return 1 if has_errors or (has_warnings and getattr(args, "strict", False)) else 0
