from __future__ import annotations

from repomod.dependencies.adapters.cargo import CargoAdapter
from repomod.dependencies.model import Adapter

ADAPTERS: tuple[Adapter, ...] = (CargoAdapter(),)
