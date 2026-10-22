from __future__ import annotations

from repomod.rules.actions import action_rules
from repomod.rules.binary_release import binary_release_rules
from repomod.rules.languages import language_rules
from repomod.rules.release import release_rules
from repomod.rules.template import template_rules
from repomod.types import Finding, Repository, Rule, TagPredicate


def _topics_placeholder(root, repository: Repository) -> list[Finding]:
    return []


def rules() -> tuple[Rule, ...]:
    return (
        *action_rules(),
        *binary_release_rules(),
        *language_rules(),
        *template_rules(),
        *release_rules(),
        Rule(
            "topics",
            "Compare GitHub repository topics with catalog tags",
            TagPredicate(),
            _topics_placeholder,
            None,
            None,
            (),
        ),
    )
