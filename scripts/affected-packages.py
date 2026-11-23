#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Compute affected Cargo packages and web-check trigger for a git diff.

Pure Python 3 standard library only; works offline using `cargo metadata --no-deps`.
Fails closed to `mode=full` when global build, toolchain, or workflow inputs change.

Usage:
    scripts/affected-packages.py [--base REF] [--committed-only] [--format json|shell] [--root DIR]
"""
from __future__ import annotations

import argparse
import json
import re
import shlex
import subprocess
import sys
from pathlib import Path

try:
    import tomllib as _tomllib
except ImportError:  # Python 3.10 fallback
    _tomllib = None

GLOBAL_TRIGGERS = (
    "rust-toolchain.toml",
    ".config/nextest.toml",
    ".github/workflows/ci.yml",
    "clippy.toml",
    "rustfmt.toml",
    "deny.toml",
    "scripts/check.sh",
    "scripts/affected-packages.py",
    "scripts/check-layers.py",
)
GLOBAL_PREFIXES = ("vendor/", ".cargo/", "scripts/public-export/")
WEB_PREFIXES = (
    "web/",
    "crates/erp-ui-compiler/",
    "crates/erp-ui-compiler-cli/",
    "crates/erp-store-wire/",
    "crates/erp-views/",
)
RUST_PREFIXES = ("crates/", "addons/", "examples/", "wit/", "fuzz/", "testdata/", "optional-assets/")
MEMBER_LINE_RE = re.compile(r'^[+-]\s*"((?:addons|crates)/[A-Za-z0-9_/-]+)",?\s*$')
WORKSPACE_DEP_LINE_RE = re.compile(
    r'^[+-]\s*erp-(?:addon|suite)-[a-z0-9_-]+\s*=\s*\{\s*path\s*=\s*"((?:addons|crates)/[^"]+)"'
)
DEPENDS_RE = re.compile(r"(?ms)^\s*depends\s*=\s*\[(.*?)\]")


def run_cmd(argv: list[str], cwd: Path, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(argv, cwd=cwd, check=check, capture_output=True, text=True)


def parse_manifest_depends(manifest_path: Path) -> list[str]:
    text = manifest_path.read_text(encoding="utf-8")
    if _tomllib is not None:
        data = _tomllib.loads(text)
        deps = data.get("depends", [])
        if not isinstance(deps, list) or not all(isinstance(d, str) for d in deps):
            raise ValueError(f"invalid depends in {manifest_path}")
        return list(deps)
    m = DEPENDS_RE.search(text)
    if not m:
        return []
    return re.findall(r'"([^"]+)"', m.group(1))


def changed_files(root: Path, base: str, committed_only: bool) -> tuple[list[str] | None, str]:
    rev = run_cmd(["git", "rev-parse", "--verify", "--quiet", base], cwd=root, check=False)
    if rev.returncode != 0:
        return None, f"base ref {base!r} is not available locally"
    diff = run_cmd(["git", "diff", "--name-only", f"{base}...HEAD"], cwd=root, check=False)
    if diff.returncode != 0:
        return None, f"git diff against {base!r} failed"
    paths = {ln.strip() for ln in diff.stdout.splitlines() if ln.strip()}
    if not committed_only:
        for cmd in (
            ["git", "diff", "--name-only"],
            ["git", "diff", "--cached", "--name-only"],
            ["git", "ls-files", "--others", "--exclude-standard"],
        ):
            extra = run_cmd(cmd, cwd=root, check=False)
            if extra.returncode != 0:
                return None, f"{' '.join(cmd)} failed"
            paths.update(ln.strip() for ln in extra.stdout.splitlines() if ln.strip())
    return sorted(paths), ""


def root_cargo_toml_only_touches_known_members(root: Path, base: str, committed_only: bool, changed: set[str]) -> bool:
    cmds = [["git", "diff", "-U0", f"{base}...HEAD", "--", "Cargo.toml"]]
    if not committed_only:
        cmds.append(["git", "diff", "-U0", "HEAD", "--", "Cargo.toml"])
    diff_lines: list[str] = []
    for cmd in cmds:
        res = run_cmd(cmd, cwd=root, check=False)
        if res.returncode != 0:
            return False
        diff_lines.extend(res.stdout.splitlines())
    touched_dirs = {
        "/".join(p.split("/")[:2])
        for p in changed
        if p.startswith(("addons/", "crates/")) and len(p.split("/")) >= 2
    }
    saw_change = False
    for line in diff_lines:
        if not line or line.startswith(("+++", "---", "@@", "diff ", "index ")):
            continue
        if line[0] not in "+-":
            continue
        saw_change = True
        m = MEMBER_LINE_RE.match(line) or WORKSPACE_DEP_LINE_RE.match(line)
        if not m:
            return False
        if m.group(1).rstrip("/") not in touched_dirs:
            return False
    return saw_change


def load_cargo_metadata(root: Path, metadata_file: Path | None = None) -> dict:
    if metadata_file is not None:
        return json.loads(metadata_file.read_text(encoding="utf-8"))
    res = run_cmd(["cargo", "metadata", "--no-deps", "--format-version", "1"], cwd=root, check=True)
    return json.loads(res.stdout)


def addon_reverse_deps(root: Path) -> dict[str, set[str]]:
    addons_dir = root / "addons"
    rdeps: dict[str, set[str]] = {}
    if not addons_dir.is_dir():
        return rdeps
    for manifest in sorted(addons_dir.glob("*/manifest.toml")):
        addon = manifest.parent.name
        for dep in parse_manifest_depends(manifest):
            rdeps.setdefault(dep, set()).add(addon)
    return rdeps


def expand_transitive(seeds: set[str], rdeps: dict[str, set[str]]) -> set[str]:
    seen = set(seeds)
    queue = list(seeds)
    while queue:
        cur = queue.pop()
        for nxt in rdeps.get(cur, ()):
            if nxt not in seen:
                seen.add(nxt)
                queue.append(nxt)
    return seen


def plan(
    root: Path,
    changed: list[str] | None,
    metadata: dict,
    *,
    base: str = "origin/main",
    committed_only: bool = False,
    missing_reason: str = "",
) -> dict:
    root = root.resolve()
    members = set(metadata.get("workspace_members", []))
    packages = [p for p in metadata.get("packages", []) if not members or p["id"] in members]
    pkg_names = {p["name"] for p in packages}
    pkg_dirs: list[tuple[str, str]] = []
    cargo_rdeps: dict[str, set[str]] = {}
    for p in packages:
        manifest_dir = Path(p["manifest_path"]).resolve().parent
        try:
            rel_dir = manifest_dir.relative_to(root).as_posix()
        except ValueError:
            rel_dir = ""
        if rel_dir:
            pkg_dirs.append((rel_dir, p["name"]))
        for dep in p.get("dependencies", []):
            dep_name = dep["name"]
            if dep_name in pkg_names:
                cargo_rdeps.setdefault(dep_name, set()).add(p["name"])
    # Match longest prefix first so nested fixture crates resolve before parent crates.
    pkg_dirs.sort(key=lambda item: len(item[0]), reverse=True)

    if changed is None:
        return {
            "mode": "full",
            "run_rust": True,
            "run_web": True,
            "packages": sorted(pkg_names),
            "reason": missing_reason or "base diff unavailable",
        }

    changed_set = set(changed)
    if not changed_set:
        return {
            "mode": "affected",
            "run_rust": False,
            "run_web": False,
            "packages": [],
            "reason": "no files changed",
        }

    for path in changed:
        if path in GLOBAL_TRIGGERS or any(path.startswith(pref) for pref in GLOBAL_PREFIXES):
            return {
                "mode": "full",
                "run_rust": True,
                "run_web": True,
                "packages": sorted(pkg_names),
                "reason": f"global input changed: {path}",
            }

    if "Cargo.toml" in changed_set:
        if not root_cargo_toml_only_touches_known_members(root, base, committed_only, changed_set):
            return {
                "mode": "full",
                "run_rust": True,
                "run_web": True,
                "packages": sorted(pkg_names),
                "reason": "root Cargo.toml changed outside member additions",
            }

    run_web = any(
        p.startswith(WEB_PREFIXES) or (p.startswith("addons/") and "/static/" in p)
        for p in changed
    )

    direct_pkgs: set[str] = set()
    direct_addons: set[str] = set()
    unmapped_rust_paths: list[str] = []
    for path in changed:
        if path in ("Cargo.toml", "Cargo.lock"):
            continue
        if path.startswith("addons/"):
            parts = path.split("/")
            if len(parts) >= 2:
                direct_addons.add(parts[1])
        matched = False
        for rel_dir, name in pkg_dirs:
            if path == rel_dir or path.startswith(rel_dir + "/"):
                direct_pkgs.add(name)
                matched = True
                break
        if not matched and (path.startswith(RUST_PREFIXES) or path.endswith(".rs")):
            # Data-only addons under addons/<name>/ have no Cargo.toml; they are tracked via direct_addons.
            if not (path.startswith("addons/") and len(path.split("/")) >= 2):
                unmapped_rust_paths.append(path)

    if unmapped_rust_paths:
        return {
            "mode": "full",
            "run_rust": True,
            "run_web": run_web,
            "packages": sorted(pkg_names),
            "reason": f"shared workspace input changed: {unmapped_rust_paths[0]}",
        }

    if "Cargo.lock" in changed_set:
        return {
            "mode": "full",
            "run_rust": True,
            "run_web": run_web,
            "packages": sorted(pkg_names),
            "reason": "Cargo.lock changed; running full Rust suite",
        }

    # Expand addon manifest reverse dependencies so XML/CSV/TOML edits pull in dependent addons & suites.
    try:
        rdeps = addon_reverse_deps(root)
    except (OSError, ValueError) as exc:
        return {
            "mode": "full",
            "run_rust": True,
            "run_web": True,
            "packages": sorted(pkg_names),
            "reason": f"failed to parse addon manifest: {exc}",
        }
    affected_addons = expand_transitive(direct_addons, rdeps)
    for addon in affected_addons:
        for candidate in (f"erp-addon-{addon}", f"erp-suite-{addon}"):
            if candidate in pkg_names:
                direct_pkgs.add(candidate)
    # If an addon is data-only and has no dedicated suite crate, include erp-modules / erp-cli smoke.
    if affected_addons and not direct_pkgs:
        for fallback in ("erp-modules", "erp-cli"):
            if fallback in pkg_names:
                direct_pkgs.add(fallback)

    affected_pkgs = sorted(expand_transitive(direct_pkgs, cargo_rdeps))
    run_rust = bool(affected_pkgs)
    reason = (
        f"{len(affected_pkgs)} affected package(s)"
        if run_rust
        else ("web-only change" if run_web else "docs or non-code change")
    )
    return {
        "mode": "affected",
        "run_rust": run_rust,
        "run_web": run_web,
        "packages": affected_pkgs,
        "reason": reason,
    }


def shard_packages(metadata: dict, shard: str) -> list[str]:
    members = set(metadata.get("workspace_members", []))
    names = sorted(p["name"] for p in metadata.get("packages", []) if not members or p["id"] in members)
    if shard == "addons":
        return [n for n in names if n.startswith("erp-addon-")]
    if shard == "suites":
        return [n for n in names if n.startswith("erp-suite-") or n == "erp-test-models"]
    if shard == "cli":
        return [n for n in names if n in ("erp-cli", "erp-ui-compiler-cli")]
    if shard == "core":
        return [
            n
            for n in names
            if not n.startswith(("erp-addon-", "erp-suite-"))
            and n not in ("erp-test-models", "erp-cli", "erp-ui-compiler-cli")
        ]
    raise ValueError(f"unknown shard {shard!r}")


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(description="Compute affected Cargo packages and web-check trigger.")
    ap.add_argument("--base", default="origin/main", help="git base ref to diff against (default: origin/main)")
    ap.add_argument("--committed-only", action="store_true", help="diff base...HEAD only (ignore unstaged files)")
    ap.add_argument("--shard", choices=("core", "addons", "suites", "cli"), help="print space-separated packages for a full-CI shard")
    ap.add_argument("--format", choices=("json", "shell"), default="json")
    ap.add_argument("--root", type=Path, default=Path(__file__).resolve().parent.parent)
    ap.add_argument("--metadata", type=Path, default=None, help="pre-captured cargo metadata JSON (for tests)")
    args = ap.parse_args(argv)

    root = args.root.resolve()
    metadata = load_cargo_metadata(root, args.metadata)
    if args.shard:
        print(" ".join(shard_packages(metadata, args.shard)))
        return 0
    changed, missing_reason = changed_files(root, args.base, args.committed_only)
    result = plan(
        root,
        changed,
        metadata,
        base=args.base,
        committed_only=args.committed_only,
        missing_reason=missing_reason,
    )

    if args.format == "json":
        print(json.dumps(result))
    else:
        print(f"MODE={shlex.quote(result['mode'])}")
        print(f"RUN_RUST={'1' if result['run_rust'] else '0'}")
        print(f"RUN_WEB={'1' if result['run_web'] else '0'}")
        print(f"REASON={shlex.quote(result['reason'])}")
        quoted = " ".join(shlex.quote(p) for p in result["packages"])
        print(f"RUST_PKGS=({quoted})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
