from __future__ import annotations

from repomod.dependencies.adapters.cargo import CargoAdapter
from repomod.dependencies.adapters.go import GoAdapter
from repomod.dependencies.adapters.javascript import JavaScriptAdapter
from repomod.dependencies.adapters.python import PythonAdapter
from repomod.dependencies.adapters.swift import SwiftAdapter
from repomod.dependencies.adapters.zig import ZigAdapter
from repomod.dependencies.model import Adapter

ADAPTERS: tuple[Adapter, ...] = (
    CargoAdapter(),
    PythonAdapter(),
    JavaScriptAdapter(),
    GoAdapter(),
    SwiftAdapter(),
    ZigAdapter(),
)
