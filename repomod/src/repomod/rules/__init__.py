from __future__ import annotations

from repomod.rules.actions import action_rules
from repomod.rules.release import release_rules
from repomod.rules.template import template_rules
from repomod.types import Finding, Repository, Rule


def _topics_placeholder(root, repository: Repository) -> list[Finding]:
    return []


def rules() -> tuple[Rule, ...]:
    return (
        *action_rules(),
        *template_rules(),
        *release_rules(),
        Rule(
            "topics",
            "Compare GitHub repository topics with catalog tags",
            frozenset(),
            _topics_placeholder,
            None,
            None,
            (),
        ),
    )
