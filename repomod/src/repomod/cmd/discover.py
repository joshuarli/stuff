from __future__ import annotations

import argparse

from repomod import catalog as catalog_module
from repomod import github

from . import common


def _discover(args: argparse.Namespace) -> int:
    catalog = common._catalog()
    if args.owner:
        seen: dict[str, str] = {}
        for owner in args.owner:
            catalog_module.validate_identifier(owner, "owner")
            key = owner.casefold()
            if key not in seen:
                seen[key] = owner
        owner_names = sorted(seen.values(), key=str.casefold)
    else:
        owner_names = sorted(
            {repo.id.owner for repo in catalog.repositories}, key=str.casefold
        )
    found = github.discover(tuple(owner_names))
    catalogued = {str(repo.id).casefold(): str(repo.id) for repo in catalog.repositories}
    discovered = {name.casefold(): name for name in found}
    uncatalogued = sorted(
        (name for key, name in discovered.items() if key not in catalogued),
        key=str.casefold,
    )
    missing = sorted(
        (name for key, name in catalogued.items() if key not in discovered),
        key=str.casefold,
    )
    if args.json:
        common._print_json(
            {"owners": owner_names, "uncatalogued": uncatalogued, "missing": missing}
        )
        return 0
    if uncatalogued:
        print(f"uncatalogued ({len(uncatalogued)}):")
        for name in uncatalogued:
            print(f"  {name}")
    else:
        print("uncatalogued (0): none")
    if missing:
        print(f"missing from GitHub ({len(missing)}):")
        for name in missing:
            print(f"  {name}")
    elif not uncatalogued:
        print("catalog matches GitHub")
    return 0
