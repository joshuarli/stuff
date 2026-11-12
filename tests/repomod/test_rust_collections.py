from __future__ import annotations

import os
from pathlib import Path

from repomod.git import run_git
from repomod.rules import rules
from repomod.rules.rust_collections import _check, _mask_noncode
from repomod.types import RepoId, Repository


def repository(*tags: str) -> Repository:
    return Repository(RepoId("acme", "demo"), tuple(tags))


def initialize(root: Path) -> None:
    root.mkdir(parents=True, exist_ok=True)
    run_git(["init", "--quiet"], cwd=root)


def write(root: Path, relative: str, source: str) -> None:
    path = root / relative
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(source, encoding="utf-8")
    run_git(["add", "--force", relative], cwd=root)


def test_rule_is_registered() -> None:
    assert "rust-collections" in {rule.id for rule in rules()}


def test_bans_btree_map_and_set_with_positions(tmp_path: Path) -> None:
    initialize(tmp_path)
    write(
        tmp_path,
        "src/lib.rs",
        "use std::collections::BTreeMap;\n"
        "fn main() { let _ = BTreeSet::new(); }\n",
    )

    findings = _check(tmp_path, repository("tool", "rust"))

    assert [(finding.file, finding.line, finding.column) for finding in findings] == [
        ("src/lib.rs", 1, 23),
        ("src/lib.rs", 2, 21),
    ]
    assert all(finding.rule == "rust-collections" for finding in findings)
    assert "rustc-hash" in findings[0].message
    assert "rapidhash::fast" in findings[0].message
    assert "foldhash::fast" in findings[1].message


def test_ignores_comments_strings_raw_strings_and_other_identifiers(tmp_path: Path) -> None:
    initialize(tmp_path)
    write(
        tmp_path,
        "src/lib.rs",
        "// BTreeMap\n"
        "/* BTreeSet /* nested BTreeMap */ */\n"
        'const DOC: &str = "BTreeMap";\n'
        'const RAW: &str = r#"BTreeSet"#;\n'
        'const BYTE: &[u8] = br#"BTreeMap"#;\n'
        "struct MyBTreeMap;\n"
        "fn main() { let _ = MyBTreeMap; }\n",
    )

    assert _check(tmp_path, repository("tool", "rust")) == []


def test_char_literal_containing_a_quote_does_not_hide_code(tmp_path: Path) -> None:
    initialize(tmp_path)
    write(
        tmp_path,
        "src/lib.rs",
        "const QUOTE: char = '\"';\nuse std::collections::BTreeMap;\n",
    )

    findings = _check(tmp_path, repository("tool", "rust"))

    assert [(finding.line, finding.column) for finding in findings] == [(2, 23)]


def test_skips_paths_excluded_from_the_cargo_workspace(tmp_path: Path) -> None:
    initialize(tmp_path)
    (tmp_path / "Cargo.toml").write_text(
        '[workspace]\nexclude = ["vendor"]\n', encoding="utf-8"
    )
    write(tmp_path, "vendor/dep/src/lib.rs", "use std::collections::BTreeMap;\n")

    assert _check(tmp_path, repository("tool", "rust")) == []


def test_skips_forks(tmp_path: Path) -> None:
    initialize(tmp_path)
    write(tmp_path, "src/lib.rs", "use std::collections::BTreeMap;\n")

    assert _check(tmp_path, repository("tool", "rust", "fork")) == []


def test_symlinked_source_is_an_operational_finding(tmp_path: Path) -> None:
    initialize(tmp_path)
    real = tmp_path / "src" / "real.rs"
    real.parent.mkdir(parents=True)
    real.write_text("use std::collections::BTreeMap;\n", encoding="utf-8")
    os.symlink("real.rs", tmp_path / "src" / "link.rs")
    run_git(["add", "--force", "src/real.rs", "src/link.rs"], cwd=tmp_path)

    findings = _check(tmp_path, repository("tool", "rust"))

    assert [(finding.file, finding.kind) for finding in findings] == [
        ("src/link.rs", "operational"),
        ("src/real.rs", "violation"),
    ]


def test_mask_preserves_offsets_for_nested_comments() -> None:
    source = "let a = 1; /* x /* y */ BTreeMap */ let b = BTreeMap;\n"

    masked = _mask_noncode(source)

    assert len(masked) == len(source)
    assert masked.endswith("let b = BTreeMap;\n")
    assert "BTreeMap" not in masked[: masked.index("let b")]
