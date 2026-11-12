from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FLEET = ROOT / "fleet"
POLICY = FLEET / "policy"
CATALOG = FLEET / "catalog.toml"
REGISTRIES = FLEET / "registries.toml"
DEPENDENCY_ROLES = FLEET / "dependency-roles.toml"
INVENTORY = FLEET / "dependencies" / "v1"
MUSL_IMAGE = ROOT / "docker" / "musl"
DASHBOARD_TS = ROOT / "repomod" / "dependencies" / "dashboard_ts"
WORK = ROOT / ".work"
SCRATCH = WORK / "tmp"
