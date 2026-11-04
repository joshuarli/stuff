import argparse
import os
import sys
from pathlib import Path

from . import adopt as adopt_module
from . import acquire, lockfile, migrate, server, validate
from .config import Config
from .errors import Error, InputError, IntegrityError, OperationError
from .names import validate_crate_name
from .pins import PinEntry, Pins, read_pins, write_pins
from .publish import Publisher
from .semver import Version
from .store import Store
from .upstream import UpstreamClient, check_url_security



class Parser(argparse.ArgumentParser):
    def error(self, message):
        raise InputError(f"{message}\n{self.format_usage().strip()}")


def _default_dir(name):
    return Path.cwd() / ".work" / name


def _add_state(parser):
    parser.add_argument("--state-dir", type=Path, default=_default_dir("state"))


def _add_site(parser):
    parser.add_argument("--site-dir", type=Path, default=_default_dir("site"))


def _add_config(parser):
    parser.add_argument("--config", type=Path)


def _load_config(args):
    return Config.load(args.config or args.state_dir / "config.json")


def build_parser():
    parser = Parser(prog="mycrates", allow_abbrev=False, description="Cargo registry acquisition and publication")
    commands = parser.add_subparsers(dest="command", required=True, parser_class=Parser)

    def command(name):
        sub = commands.add_parser(name, allow_abbrev=False)
        return sub

    sub = command("init")
    _add_state(sub)
    _add_site(sub)

    sub = command("config")
    actions = sub.add_subparsers(dest="action", required=True, parser_class=Parser)
    action = actions.add_parser("set-public-base-url", allow_abbrev=False)
    action.add_argument("url")
    _add_state(action)

    sub = command("plan")
    _add_state(sub)

    sub = command("import-lock")
    sub.add_argument("lockfile", type=Path)
    sub.add_argument("--project")
    _add_state(sub)

    sub = command("add")
    sub.add_argument("target")
    _add_state(sub)
    _add_config(sub)

    sub = command("adopt")
    sub.add_argument("--from-site", type=Path, required=True)
    _add_state(sub)

    sub = command("fetch")
    _add_state(sub)
    _add_config(sub)

    sub = command("publish")
    _add_state(sub)
    _add_site(sub)
    _add_config(sub)

    sub = command("serve-publish")
    sub.add_argument("--bind", default="127.0.0.1:8080")
    sub.add_argument("--token-file", type=Path)
    _add_state(sub)
    _add_site(sub)
    _add_config(sub)

    sub = command("verify")
    _add_state(sub)

    sub = command("doctor")
    _add_state(sub)

    sub = command("validate")
    sub.add_argument("--project")
    sub.add_argument("--project-dir", type=Path)
    sub.add_argument("--manifest-path", type=Path)
    sub.add_argument("--mirror-url")
    _add_state(sub)
    _add_config(sub)

    sub = command("migrate-state")
    _add_state(sub)
    return parser


def cmd_init(args):
    store = Store(args.state_dir)
    store.ensure_dirs()
    args.site_dir.mkdir(parents=True, exist_ok=True)
    config_path = args.state_dir / "config.json"
    if not config_path.exists():
        Config().save(config_path)
    print(f"initialized state at {args.state_dir}")


def cmd_config(args):
    check_url_security(args.url)
    config_path = args.state_dir / "config.json"
    config = Config.load(config_path)
    config.public_base_url = args.url
    config.save(config_path)
    print(f"set public_base_url to {args.url}")


def cmd_plan(args):
    items = acquire.plan(args.state_dir)
    if not items:
        print("plan: no packages to fetch")
        return
    print(f"plan: {len(items)} packages to fetch:")
    for item in items:
        print(f"  {item.name}@{item.version} ({item.checksum})")


def cmd_import_lock(args):
    lock_file = args.lockfile
    if not lock_file.exists():
        raise InputError(f"lockfile not found: {lock_file}")
    lock_text = lock_file.read_text(encoding="utf-8")
    parsed = lockfile.parse_lockfile(lock_text)
    project = args.project or lock_file.parent.name
    if project in ("", ".."):
        project = "default"
    if "/" in project or "\0" in project or project == ".":
        raise InputError(f"invalid project name '{project}'")
    registry = []
    unsupported = []
    for package in parsed.packages:
        kind = lockfile.classify(package)
        label = f"{package.name} {package.version}"
        if kind == "crates-io":
            if package.checksum is None:
                raise IntegrityError(f"package {label} missing checksum in Cargo.lock")
            registry.append(PinEntry(package.name, package.version, package.checksum, package.source))
        elif kind == "git":
            unsupported.append(f"git: {label}")
        elif kind == "alternate-registry":
            unsupported.append(f"alternate-registry ({package.source}): {label}")
        elif kind == "path":
            unsupported.append(f"path: {label}")
        elif kind == "unsupported":
            unsupported.append(f"unsupported: {label}")
    pins = Pins(project, str(lock_file), lock_text, registry, unsupported)
    store = Store(args.state_dir)
    store.ensure_dirs()
    write_pins(store.pins_dir / f"{project}.json", pins)
    print(f"imported {len(registry)} registry packages for project '{project}'")
    if unsupported:
        print(f"warning: {len(unsupported)} unsupported non-crates.io sources:")
        for item in unsupported:
            print(f"  {item}")


def cmd_add(args):
    name, separator, version_text = args.target.partition("@")
    if not separator:
        raise InputError("expected <name>@<version> format")
    validate_crate_name(name)
    version = str(Version.parse(version_text))
    config = _load_config(args)
    store = Store(args.state_dir)
    store.ensure_dirs()
    store.assert_current_format()
    pins_path = store.pins_dir / "default.json"
    pins = read_pins(pins_path) if pins_path.exists() else Pins()
    if any(p.name == name and p.version == version for p in pins.registry):
        print(f"already pinned {name}@{version}")
        return
    client = UpstreamClient(config.upstream.index_url, config.limits)
    _, entry = client.fetch_index_entry(name, version)
    pins.registry.append(PinEntry(name, version, entry.cksum, lockfile.CRATES_IO_INDEX_SOURCE))
    write_pins(pins_path, pins)
    print(f"added {name}@{version}")


def cmd_adopt(args):
    summary = adopt_module.adopt_site(Store(args.state_dir), args.from_site)
    print(
        f"adopted {summary.native} native and {summary.upstream} upstream index entries "
        f"({summary.blobs} new blobs)"
    )


def cmd_fetch(args):
    summary = acquire.fetch(args.state_dir, _load_config(args))
    print(f"fetched {summary.fetched_count} packages ({summary.total_bytes} bytes)")


def cmd_publish(args):
    config = _load_config(args)
    store = Store(args.state_dir)
    with store.lock_exclusive():
        result = Publisher(store, args.site_dir, config.public_base_url, config.auth_required).publish()
    print(f"published generation {result.generation_id} (noop: {str(result.is_noop).lower()})")


def cmd_serve_publish(args):
    config = _load_config(args)
    host, port = server.split_bind(args.bind)
    if args.token_file is not None:
        if not args.token_file.exists():
            raise InputError(f"token file not found: {args.token_file}")
        mode = args.token_file.stat().st_mode & 0o777
        if mode & 0o077:
            raise InputError(f"token file permissions must be 0600 (got {mode:o})")
        token = args.token_file.read_text(encoding="utf-8").strip()
    else:
        token = os.environ.get("MYCRATES_TOKEN", "")
    if not token:
        raise InputError("refusing to serve the publish API without a token: set --token-file or MYCRATES_TOKEN")
    serve_config = server.ServeConfig(
        bind_host=host,
        bind_port=port,
        token=token,
        auth_required=config.auth_required,
        state_dir=args.state_dir,
        site_dir=args.site_dir,
        public_base_url=config.public_base_url,
        limits=config.limits,
        body_timeout=max(config.limits.upload_timeout_secs, 30),
    )
    httpd = server.PublishServer(serve_config)
    print(f"listening on {httpd.server_address[0]}:{httpd.server_address[1]}", flush=True)
    try:
        httpd.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        httpd.server_close()


def cmd_verify(args):
    acquire.verify(args.state_dir)
    print("verification passed: all blobs and records intact")


def cmd_doctor(args):
    state_dir = args.state_dir
    if not state_dir.exists():
        print(f"doctor: state directory does not exist at {state_dir}")
        return
    store = Store(state_dir)
    store.assert_current_format()
    issues = 0
    if store.pins_dir.exists():
        for path in sorted(store.pins_dir.glob("*.json")):
            pins = read_pins(path)
            project = pins.project or str(path)
            if pins.unsupported:
                print(f"project '{project}' has {len(pins.unsupported)} unsupported sources:")
                for item in pins.unsupported:
                    print(f"  [unsupported] {item}")
                    issues += 1
            for pin in pins.registry:
                if not store.blob_path(pin.checksum).exists():
                    print(f"  [missing blob] {pin.name} {pin.version} ({pin.checksum})")
                    issues += 1
    if issues == 0:
        print("doctor: system healthy, no missing artifacts or unsupported sources detected")
    else:
        print(f"doctor: detected {issues} issues")


def cmd_validate(args):
    if args.project_dir is not None and args.manifest_path is not None:
        raise InputError("validate: supply only one of --project-dir and --manifest-path")
    project_source = args.project_dir or args.manifest_path
    config_path = args.config or args.state_dir / "config.json"
    config = Config.load(config_path) if config_path.exists() else Config()
    mirror = args.mirror_url or f"{config.public_base_url.rstrip('/')}/current/crates-io/"
    store = Store(args.state_dir)
    store.assert_current_format()
    if not store.pins_dir.exists():
        print(f"validate: no pinned projects found in {store.pins_dir}")
        return
    validated = 0
    for path in sorted(store.pins_dir.glob("*.json")):
        if args.project is not None and args.project != path.stem:
            continue
        result = validate.validate_project(args.state_dir, path, mirror, project_source)
        print(
            f"project '{result.project}': {result.registry_count} registry packages verified "
            f"(partial: {str(result.is_partial).lower()})"
        )
        validated += 1
    if validated == 0 and args.project is not None:
        raise InputError(f"project '{args.project}' not found in pins")


def cmd_migrate_state(args):
    summary = migrate.migrate_state(args.state_dir)
    print(f"migrated {summary.native} native records and {summary.pins} pin files to JSON")


COMMANDS = {
    "init": cmd_init,
    "config": cmd_config,
    "plan": cmd_plan,
    "import-lock": cmd_import_lock,
    "add": cmd_add,
    "adopt": cmd_adopt,
    "fetch": cmd_fetch,
    "publish": cmd_publish,
    "serve-publish": cmd_serve_publish,
    "verify": cmd_verify,
    "doctor": cmd_doctor,
    "validate": cmd_validate,
    "migrate-state": cmd_migrate_state,
}


def main(argv=None):
    try:
        args = build_parser().parse_args(sys.argv[1:] if argv is None else argv)
        COMMANDS[args.command](args)
    except Error as error:
        print(error, file=sys.stderr)
        return error.exit_code
    except OSError as error:
        print(OperationError(str(error)), file=sys.stderr)
        return OperationError.exit_code
    return 0
