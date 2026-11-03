from __future__ import annotations

import argparse
from collections.abc import Sequence

from repomod import catalog as catalog_module
from repomod import github

from . import common


def _discover(args: argparse.Namespace) -> int:
    catalog = common.default_catalog()
    if args.owner:
        seen: dict[str, str] = {}
        for owner in args.owner:
            catalog_module.validate_identifier(owner, "owner")
            key = owner.casefold()
            if key not in seen:
                seen[key] = owner
        owner_names = sorted(seen.values(), key=str.casefold)
    else:
        owners = [repo.id.owner for repo in (*catalog.repositories, *catalog.ignored)]
        owners.extend(repo.owner for repo in catalog.pending)
        owner_names = sorted({owner.casefold(): owner for owner in owners}.values(), key=str.casefold)
    found = github.discover(tuple(owner_names))
    selected_owners = {owner.casefold() for owner in owner_names}
    catalogued = {
        str(repo.id).casefold(): str(repo.id)
        for repo in catalog.repositories
        if repo.id.owner.casefold() in selected_owners
    }
    discovered = {name.casefold(): name for name in found}
    known = set(catalogued)
    known.update(str(repo.id).casefold() for repo in catalog.ignored)
    known.update(str(repo).casefold() for repo in catalog.pending)
    uncatalogued = sorted(
        (name for key, name in discovered.items() if key not in known),
        key=str.casefold,
    )
    missing = sorted(
        (name for key, name in catalogued.items() if key not in discovered),
        key=str.casefold,
    )
    uncatalogued = catalog_module.add_pending(catalog, uncatalogued)
    pending = sorted(
        (str(repo) for repo in catalog.pending if repo.owner.casefold() in selected_owners),
        key=str.casefold,
    )
    pending = sorted([*pending, *uncatalogued], key=str.casefold)
    render_discover(owner_names, uncatalogued, missing, pending, as_json=args.json)
    return 0


def render_discover(
    owners: Sequence[str],
    uncatalogued: Sequence[str],
    missing: Sequence[str],
    pending: Sequence[str],
    *,
    as_json: bool,
) -> None:
    if as_json:
        common.print_json(
            {
                "owners": list(owners),
                "uncatalogued": list(uncatalogued),
                "missing": list(missing),
                "pending": list(pending),
            }
        )
        return
    if uncatalogued:
        print(f"added to pending ({len(uncatalogued)}):")
        for name in uncatalogued:
            print(f"  {name}")
    else:
        print("added to pending (0): none")
    if pending:
        print(f"pending review ({len(pending)}):")
        for name in pending:
            print(f"  {name}")
    if missing:
        print(f"missing from GitHub ({len(missing)}):")
        for name in missing:
            print(f"  {name}")
    elif not pending:
        print("catalog matches GitHub")
