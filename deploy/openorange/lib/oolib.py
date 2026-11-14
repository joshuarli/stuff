"""Shared configuration, naming and docker-argument construction for the
OpenOrange host stack. Standard library only (Python >= 3.11 for tomllib).

Everything that must agree between OpenRC units, health checks, backups and
the ingress configuration is derived here from `apps.toml` and
`images.lock.json`; no other file repeats an address, name or image reference.
"""
from __future__ import annotations

import ipaddress
import json
import os
import re
import secrets
import subprocess
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
NAME_RE = re.compile(r"^[a-z][a-z0-9-]{1,30}$")
HOST_RE = re.compile(r"^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z]{2,}$")
BUCKET_RE = re.compile(r"^[a-z0-9][a-z0-9.-]{2,62}$")
UID = "10001"  # unprivileged uid:gid inside the ingress and identity containers

ENGINE_PORT = 6420
STORE_PORT = 7070

# Reserved: engine admin namespace seeded by the engine itself.
RESERVED_NAMESPACES = {"default"}

# open-orange 1.0.2 hard-codes its pool names (`primaryRivetPoolName = "default"` for
# the Worker's Durable Objects, `services` for its internal services process);
# it cannot be pointed at another pool. Pools are namespace-scoped, so each
# app's isolation is its namespace, and both pools are created in every one.
ORANGE_POOLS = ("default", "services")
PRIMARY_POOL = "default"


class ConfigError(Exception):
    pass


def _only(d: dict, allowed: set[str], where: str) -> None:
    extra = set(d) - allowed
    if extra:
        raise ConfigError(f"{where}: unknown keys {sorted(extra)}")


def _req(d: dict, key: str, typ, where: str):
    if key not in d:
        raise ConfigError(f"{where}: missing `{key}`")
    if not isinstance(d[key], typ) or isinstance(d[key], bool) != (typ is bool):
        raise ConfigError(f"{where}.{key}: expected {typ.__name__}")
    return d[key]


@dataclass(frozen=True)
class App:
    name: str
    kind: str  # "orange" | "native"
    display: str
    hostnames: tuple[str, ...]
    enabled: bool = True
    namespace: str = ""
    ip: str = ""
    port: int = 0
    buckets: tuple[str, ...] = ()
    upstream: str = ""
    image: str = ""  # repository name for local builds, e.g. "oo-demo"
    memory: str = "1g"
    cpus: float = 1.0
    health_path: str = "/"
    secrets: tuple[str, ...] = ()  # extra Worker secret names, values in apps/NAME.extra.env
    # Hostnames (a subset of `hostnames`) also served on the loopback public listener, for traffic
    # that arrives through an outbound-only tunnel. Everything else is tailnet-only.
    public_hostnames: tuple[str, ...] = ()
    public_deny_paths: tuple[str, ...] = ("/admin",)  # each entry and `entry/*` answer 404 publicly

    @property
    def container(self) -> str:
        return f"oo-app-{self.name}"

    @property
    def service(self) -> str:
        return f"oo-app.{self.name}"

    @property
    def s3_user(self) -> str:
        return f"app-{self.name}"

    @property
    def env_key(self) -> str:
        return self.name.upper().replace("-", "_")


@dataclass
class Config:
    raw: dict
    stack: dict
    network: dict
    ingress: dict
    store: dict
    apps: dict[str, App] = field(default_factory=dict)

    # --- paths -----------------------------------------------------------
    def p(self, key: str, *parts: str) -> Path:
        return Path(self.stack[key]).joinpath(*parts)

    @property
    def etc(self) -> Path:
        return self.p("etc_dir")

    @property
    def state(self) -> Path:
        return self.p("state_dir")

    @property
    def run(self) -> Path:
        return self.p("run_dir")

    @property
    def logs(self) -> Path:
        return self.p("log_dir")

    def app_env(self, app: App) -> Path:
        return self.etc / "apps" / f"{app.name}.env"

    def app_extra_env(self, app: App) -> Path:
        return self.etc / "apps" / f"{app.name}.extra.env"

    def app_image_file(self, app: App) -> Path:
        return self.etc / "apps" / f"{app.name}.image"

    @property
    def engine_env(self) -> Path:
        return self.etc / "engine.env"

    @property
    def store_env(self) -> Path:
        return self.etc / "store.env"

    @property
    def engine_url(self) -> str:
        return f"http://{self.network['engine_ip']}:{ENGINE_PORT}"

    @property
    def store_url(self) -> str:
        return f"http://{self.network['store_ip']}:{STORE_PORT}"

    @property
    def public(self) -> dict:
        """The loopback public listener ({bind, port}) or {} when none is configured."""
        return self.ingress.get("public", {})

    def enabled_apps(self) -> list[App]:
        return [a for a in self.apps.values() if a.enabled]

    def orange_apps(self) -> list[App]:
        return [a for a in self.enabled_apps() if a.kind == "orange"]


def _deep_merge(a: dict, b: dict) -> dict:
    out = dict(a)
    for k, v in b.items():
        out[k] = _deep_merge(out[k], v) if isinstance(v, dict) and isinstance(out.get(k), dict) else v
    return out


def load(path: str | os.PathLike | None = None, overlay: str | os.PathLike | None = None) -> Config:
    path = Path(path or os.environ.get("OO_APPS") or ROOT / "apps.toml")
    raw = tomllib.loads(path.read_text())
    overlay = overlay or os.environ.get("OO_OVERLAY")
    if overlay is None:
        # A host-local overlay (rehearsal) lives next to the secrets.
        cand = Path(raw.get("stack", {}).get("etc_dir", "/etc/openorange")) / "overlay.toml"
        overlay = cand if cand.exists() else None
    if overlay:
        raw = _deep_merge(raw, tomllib.loads(Path(overlay).read_text()))
    return validate(raw)


def validate(raw: dict) -> Config:
    _only(raw, {"schema", "stack", "network", "ingress", "store", "apps"}, "apps.toml")
    if raw.get("schema") != 1:
        raise ConfigError("schema must be 1")
    stack, net, ing, store = (raw.get(k, {}) for k in ("stack", "network", "ingress", "store"))
    _only(stack, {"code_dir", "state_dir", "etc_dir", "log_dir", "run_dir"}, "stack")
    for k in ("code_dir", "state_dir", "etc_dir", "log_dir", "run_dir"):
        v = _req(stack, k, str, "stack")
        if not v.startswith("/"):
            raise ConfigError(f"stack.{k} must be absolute")
    _only(net, {"name", "bridge", "subnet", "internal", "store_ip", "engine_ip"}, "network")
    _req(net, "name", str, "network"), _req(net, "bridge", str, "network")
    if len(net["bridge"]) > 15:
        raise ConfigError("network.bridge longer than 15 characters")
    subnet = ipaddress.ip_network(_req(net, "subnet", str, "network"))
    _req(net, "internal", bool, "network")
    for k in ("store_ip", "engine_ip"):
        if ipaddress.ip_address(_req(net, k, str, "network")) not in subnet:
            raise ConfigError(f"network.{k} not inside subnet")
    _only(ing, {"bind", "tls", "dns_env_file", "public"}, "ingress")
    ipaddress.ip_address(_req(ing, "bind", str, "ingress"))
    if ing.get("bind") in ("0.0.0.0", "::", ""):
        raise ConfigError("ingress.bind must be a specific tailnet address, never a wildcard")
    if _req(ing, "tls", str, "ingress") not in ("internal", "acme"):
        raise ConfigError("ingress.tls must be 'internal' or 'acme'")
    _req(ing, "dns_env_file", str, "ingress")
    pub = ing.get("public", {})
    _only(pub, {"bind", "port"}, "ingress.public")
    if pub:
        pbind = ipaddress.ip_address(_req(pub, "bind", str, "ingress.public"))
        if not pbind.is_loopback:
            raise ConfigError("ingress.public.bind must be a loopback address: the public listener is reached only by the local tunnel")
        if not 1024 <= _req(pub, "port", int, "ingress.public") <= 65535:
            raise ConfigError("ingress.public.port must be 1024-65535")
    _only(store, {"region"}, "store")
    _req(store, "region", str, "store")

    apps: dict[str, App] = {}
    used_ips = {net["store_ip"], net["engine_ip"]}
    used_ns: set[str] = set(RESERVED_NAMESPACES)
    used_hosts: dict[str, str] = {}
    used_buckets: set[str] = set()
    common = {"kind", "display", "hostnames", "enabled", "health_path", "public_hostnames", "public_deny_paths"}
    orange_keys = common | {"namespace", "ip", "port", "buckets", "image", "memory", "cpus", "secrets"}
    native_keys = common | {"upstream"}
    for name, a in raw.get("apps", {}).items():
        where = f"apps.{name}"
        if not NAME_RE.match(name):
            raise ConfigError(f"{where}: invalid app name")
        kind = _req(a, "kind", str, where)
        if kind not in ("orange", "native"):
            raise ConfigError(f"{where}.kind must be 'orange' or 'native'")
        _only(a, orange_keys if kind == "orange" else native_keys, where)
        hosts = tuple(_req(a, "hostnames", list, where))
        if not hosts:
            raise ConfigError(f"{where}.hostnames is empty")
        enabled = a.get("enabled", True)
        for h in hosts:
            if not isinstance(h, str) or not HOST_RE.match(h):
                raise ConfigError(f"{where}: invalid hostname {h!r}")
            if enabled:
                if h in used_hosts:
                    raise ConfigError(f"{where}: hostname {h} already used by {used_hosts[h]}")
                used_hosts[h] = name
        pubs = tuple(a.get("public_hostnames", []))
        if pubs and not pub:
            raise ConfigError(f"{where}: public_hostnames needs an [ingress.public] table")
        if not set(pubs) <= set(hosts):
            raise ConfigError(f"{where}: public_hostnames must be a subset of hostnames: {sorted(set(pubs) - set(hosts))}")
        deny = tuple(a.get("public_deny_paths", ["/admin"]))
        if not all(isinstance(p, str) and re.match(r"^/[A-Za-z0-9._~/-]*[A-Za-z0-9._~-]$", p) for p in deny):
            raise ConfigError(f"{where}.public_deny_paths must be absolute path prefixes without a trailing slash")
        kw = dict(name=name, kind=kind, display=a.get("display", name), hostnames=hosts, enabled=enabled,
                  health_path=a.get("health_path", "/"), public_hostnames=pubs, public_deny_paths=deny)
        if kind == "orange":
            ns = _req(a, "namespace", str, where)
            ip = _req(a, "ip", str, where)
            port = _req(a, "port", int, where)
            if not NAME_RE.match(ns):
                raise ConfigError(f"{where}: invalid namespace name")
            if ipaddress.ip_address(ip) not in subnet:
                raise ConfigError(f"{where}.ip outside {subnet}")
            if not 1 <= port <= 65535:
                raise ConfigError(f"{where}.port out of range")
            buckets = tuple(a.get("buckets", []))
            for b in buckets:
                if not isinstance(b, str) or not BUCKET_RE.match(b):
                    raise ConfigError(f"{where}: invalid bucket {b!r}")
            if enabled:
                for what, val, used in (("namespace", ns, used_ns), ("ip", ip, used_ips)):
                    if val in used:
                        raise ConfigError(f"{where}: {what} {val} already used")
                    used.add(val)
                dup = set(buckets) & used_buckets
                if dup:
                    raise ConfigError(f"{where}: buckets {sorted(dup)} already belong to another app")
                used_buckets |= set(buckets)
            secrets_ = tuple(a.get("secrets", []))
            for sname in secrets_:
                if not isinstance(sname, str) or not re.match(r"^[A-Z][A-Z0-9_]{1,63}$", sname) or sname.startswith(("RIVET", "ORANGE_", "OO_", "S3_")):
                    raise ConfigError(f"{where}: invalid secret name {sname!r}")
            kw.update(secrets=secrets_, namespace=ns, ip=ip, port=port, buckets=buckets, image=a.get("image", ""),
                      memory=a.get("memory", "1g"), cpus=float(a.get("cpus", 1.0)))
        else:
            up = _req(a, "upstream", str, where)
            host, _, port = up.rpartition(":")
            ipaddress.ip_address(host)
            if not port.isdigit():
                raise ConfigError(f"{where}.upstream must be IP:PORT")
            kw.update(upstream=up)
        apps[name] = App(**kw)
    return Config(raw, stack, net, ing, store, apps)


# --- images ------------------------------------------------------------------
def lock() -> dict:
    return json.loads((ROOT / "images.lock.json").read_text())


def image_ref(key: str) -> str:
    img = lock()["images"][key]
    return f"{img['repository']}@{img['index_digest']}"


def local_tag(key: str) -> str:
    return lock()["local_builds"][key]["tag"]


# --- env files ---------------------------------------------------------------
def read_env(path: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    if not path.exists():
        return out
    for line in path.read_text().splitlines():
        line = line.strip()
        if line and not line.startswith("#") and "=" in line:
            k, v = line.split("=", 1)
            out[k] = v
    return out


def write_env(path: Path, items: dict[str, str], header: str = "") -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    body = (f"# {header}\n" if header else "") + "".join(f"{k}={v}\n" for k, v in items.items())
    tmp = path.with_name(path.name + ".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as f:
        f.write(body)
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)


def token(nbytes: int = 32) -> str:
    return secrets.token_hex(nbytes)


# --- docker run arguments ------------------------------------------------------
COMMON = ["run", "--rm", "--log-driver", "none", "--security-opt", "no-new-privileges", "--cap-drop", "ALL"]


def _net(cfg: Config, ip: str) -> list[str]:
    return ["--network", cfg.network["name"], "--ip", ip]


def engine_args(cfg: Config, name: str, ip: str, env_file: Path, config_file: Path, log_dir: Path, objects_dir: Path,
                detach: bool = False) -> list[str]:
    """`docker run` arguments for one Engine instance. Shared by the live unit and the restore rehearsal."""
    # The Engine takes an ownership lock next to its object tier under /var/lib/rivet, so that
    # directory is a tmpfs with the two data directories bind-mounted into it.
    return COMMON + (["-d"] if detach else []) + [
        "--name", name, *_net(cfg, ip), "--read-only", "--stop-timeout", "120",
        "--tmpfs", f"/var/lib/rivet:uid={UID},gid={UID},mode=0700", "--ulimit", "nofile=1048576:1048576",
        "--user", f"{UID}:{UID}", "--env-file", str(env_file),
        "-v", f"{config_file}:/etc/rivet/config.json:ro",
        "-v", f"{log_dir}:/var/lib/rivet/log", "-v", f"{objects_dir}:/var/lib/rivet/objects",
        "--tmpfs", "/tmp", image_ref("engine")]


def run_args(cfg: Config, service: str, app: str | None = None) -> list[str]:
    n = cfg.network
    if service == "store":
        # versitygw posix backend as an unprivileged user with no capabilities.
        # The admin API listens on a unix socket (not on any network) and is
        # driven from the host with the root key (see `oo provision`).
        return COMMON + ["--name", "oo-store", *_net(cfg, n["store_ip"]), "--read-only", "--user", f"{UID}:{UID}",
                         "--env-file", str(cfg.store_env),
                         "-v", f"{cfg.state}/store/data:/data", "-v", f"{cfg.state}/store/versions:/versions",
                         "-v", f"{cfg.state}/store/iam:/iam", "-v", f"{cfg.run}/store-admin:/run/adm",
                         "--tmpfs", "/tmp",
                         image_ref("store"),
                         "--port", f":{STORE_PORT}", "--admin-port", "/run/adm/admin.sock",
                         "--region", cfg.store["region"], "--iam-dir", "/iam", "--health", "/health",
                         "--quiet", "posix", "--fsync", "--versioning-dir", "/versions", "/data"]
    if service == "engine":
        return engine_args(cfg, "oo-engine", n["engine_ip"], cfg.engine_env, cfg.etc / "engine" / "config.json",
                           cfg.state / "engine" / "log", cfg.state / "engine" / "objects")
    if service == "identity":
        return COMMON + ["--name", "oo-identity", "--network", "none", "--read-only", "--user", f"{UID}:{UID}",
                         "-v", "/var/run/tailscale:/var/run/tailscale:ro",
                         "-v", f"{cfg.run}/identity:/run/identity",
                         "--memory", "64m", "--pids-limit", "32", local_tag("identity")]
    if service == "ingress":
        args = COMMON + ["--cap-add", "NET_BIND_SERVICE", "--name", "oo-ingress", "--network", "host",
                         "--read-only", "--user", f"{UID}:{UID}", "--stop-timeout", "30",
                         "--env-file", str(cfg.etc / "ingress.env"),
                         "-v", f"{cfg.etc}/ingress/Caddyfile:/etc/caddy/Caddyfile:ro",
                         "-v", f"{cfg.state}/caddy/data:/data", "-v", f"{cfg.state}/caddy/config:/config",
                         "-v", f"{cfg.run}/identity:/run/identity", "-v", f"{cfg.run}/caddy:/run/caddy",
                         "--tmpfs", "/tmp"]
        if cfg.ingress["tls"] == "acme":
            dns = Path(cfg.ingress["dns_env_file"])
            if not dns.exists():
                raise ConfigError(f"ingress.tls = acme but {dns} does not exist (CF_API_TOKEN pending)")
            args += ["--env-file", str(dns)]
        return args + [local_tag("caddy"), "caddy", "run", "--config", "/etc/caddy/Caddyfile", "--adapter", "caddyfile"]
    if service == "app":
        a = cfg.apps[app] if app else None
        if a is None or a.kind != "orange":
            raise ConfigError(f"unknown orange app {app!r}")
        img_file = cfg.app_image_file(a)
        ref = img_file.read_text().strip() if img_file.exists() else ""
        if not ref:
            raise ConfigError(f"no image deployed for {a.name}: run `oo set-image {a.name} IMAGE`")
        return COMMON + ["--name", a.container, *_net(cfg, a.ip), "--read-only", "--stop-timeout", "30",
                         "--user", "1000:1000", "--memory", a.memory, "--cpus", str(a.cpus), "--pids-limit", "512",
                         "--env-file", str(cfg.app_env(a)), *(["--env-file", str(cfg.app_extra_env(a))] if cfg.app_extra_env(a).exists() else []), "--tmpfs", "/tmp", "--tmpfs", "/app/.open-orange", "--tmpfs", "/home/node:uid=1000,gid=1000",
                         "--env", f"PORT={a.port}", ref]
    raise ConfigError(f"unknown service {service!r}")


def docker(*args: str, check: bool = True, capture: bool = True, input: str | None = None, timeout: float | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(["docker", *args], check=check, capture_output=capture, text=True, input=input, timeout=timeout)
