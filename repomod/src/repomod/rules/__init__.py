from __future__ import annotations

from pathlib import Path

from repomod.rules.actions import action_rules
from repomod.rules.audit_upgrades import audit_upgrade_rules
from repomod.rules.binary_release import binary_release_rules
from repomod.rules.consistency import consistency_rules
from repomod.rules.formatters import formatter_rules
from repomod.rules.languages import language_rules
from repomod.rules.mdtool import mdtool_rules
from repomod.rules.python_standard import python_standard_rules
from repomod.rules.release import release_rules
from repomod.rules.rust_toolchain import rust_toolchain_rules
from repomod.rules.template import template_rules
from repomod.rules.typescript_standard import typescript_standard_rules
from repomod.rules.workflow_settings import workflow_settings_rules
from repomod.types import Finding, Repository, Rule, TagPredicate


def _topics_placeholder(root: Path, repository: Repository) -> list[Finding]:
    return []


def _protection_placeholder(root: Path, repository: Repository) -> list[Finding]:
    return []


def rules() -> tuple[Rule, ...]:
    return (
        *action_rules(),
        *audit_upgrade_rules(),
        *binary_release_rules(),
        *consistency_rules(),
        *formatter_rules(),
        *language_rules(),
        *mdtool_rules(),
        *python_standard_rules(),
        *template_rules(),
        *typescript_standard_rules(),
        *workflow_settings_rules(),
        *release_rules(),
        *rust_toolchain_rules(),
        Rule(
            "topics",
            "Compare GitHub repository topics with catalog tags",
            TagPredicate(),
            _topics_placeholder,
            None,
            None,
            (),
        ),
        Rule(
            "github-protection",
            "Require zero approving reviews on protected branches",
            TagPredicate(),
            _protection_placeholder,
            None,
            None,
            (),
        ),
    )
