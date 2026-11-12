from __future__ import annotations

import pytest

from repomod.dependencies.model import Ecosystem
from repomod.dependency_roles import find_ban, load_roles, parse_roles


def _runtime() -> dict[str, object]:
    return {
        "ecosystem": "cargo",
        "preferred": "smolith",
        "options": {
            "smolith": ["laputa-smol", "laputa-async-io", "laputa-parking"],
            "smol": ["smol", "async-io", "parking"],
            "tokio": ["tokio", "tokio-util"],
        },
        "ban": {"smol": "exact", "tokio": "prefix"},
    }


def test_prefix_bans_cover_suffixed_crates_and_exact_bans_do_not() -> None:
    roles = parse_roles({"async-runtime": _runtime()}, "roles.toml")
    assert find_ban(roles, Ecosystem.CARGO, "tokio_macros") is not None
    assert find_ban(roles, Ecosystem.CARGO, "parking") is not None
    assert find_ban(roles, Ecosystem.CARGO, "parking_lot") is None
    assert find_ban(roles, Ecosystem.CARGO, "laputa-async-io") is None
    assert find_ban(roles, Ecosystem.NPM, "tokio") is None


def test_replacement_names_the_matching_fork_or_the_preferred_option() -> None:
    roles = parse_roles({"async-runtime": _runtime()}, "roles.toml")
    fork = find_ban(roles, Ecosystem.CARGO, "async_io")
    other = find_ban(roles, Ecosystem.CARGO, "tokio-util")
    assert fork is not None
    assert fork.replacement == "laputa-async-io from smolith"
    assert other is not None
    assert other.replacement == "smolith"


@pytest.mark.parametrize(
    ("ban", "preferred", "message"),
    [
        ({"smolith": "exact"}, "smolith", "must be another option"),
        ({"smol": "fuzzy"}, "smolith", "must be exact or prefix"),
        ({"smol": "exact"}, None, "need a preferred option"),
    ],
)
def test_invalid_bans_are_rejected(ban: object, preferred: str | None, message: str) -> None:
    role = {**_runtime(), "ban": ban}
    if preferred is None:
        del role["preferred"]
    with pytest.raises(ValueError, match=message):
        parse_roles({"async-runtime": role}, "roles.toml")


def test_curated_bans_keep_the_fleet_policy() -> None:
    roles = load_roles()
    for name in ("clap", "clap_builder", "tokio", "criterion-plot", "proptest", "hyper-util",
                 "reqwest", "smol-hyper", "async-std", "futures-lite", "polling"):
        assert find_ban(roles, Ecosystem.CARGO, name) is not None, name
    for name in ("lexopt", "laputa-smol", "parking_lot", "quickcheck", "rustybench", "h12tiny",
                 "laputa-hyper-futures-lite", "fastrand"):
        assert find_ban(roles, Ecosystem.CARGO, name) is None, name


def test_only_the_preferred_option_may_stand_for_the_standard_library() -> None:
    options = {"stdlib": [], "requests": ["requests"], "httpx": ["httpx"]}
    [role] = parse_roles(
        {"python-http": {"ecosystem": "pypi", "preferred": "stdlib", "options": options}},
        "roles.toml",
    )
    assert role.options["stdlib"] == frozenset()
    with pytest.raises(ValueError, match="only the preferred option"):
        parse_roles(
            {"python-http": {"ecosystem": "pypi", "preferred": "requests", "options": options}},
            "roles.toml",
        )
