#!/usr/bin/env python3
# SPDX-License-Identifier: MIT OR Apache-2.0
"""Fail when a commit between two revisions has no DCO `Signed-off-by:` line.

    scripts/check-dco.py BASE HEAD

Checks every non-merge commit reachable from HEAD and not from BASE (the commits of a pull request).
`git commit -s` adds the line; `git rebase --signoff BASE` adds it to commits already made.
"""
from __future__ import annotations

import re
import subprocess
import sys

SIGNED_OFF_BY_RE = re.compile(r"^Signed-off-by:\s+.+\s+<[^>]+>\s*$", re.M)


def git(*args: str) -> str:
    res = subprocess.run(["git", *args], capture_output=True, text=True)
    if res.returncode != 0:
        raise SystemExit(f"check-dco: git {' '.join(args)} failed: {res.stderr.strip()}")
    return res.stdout


def unsigned_commits(base: str, head: str) -> list[str]:
    shas = git("rev-list", "--no-merges", f"{base}..{head}").split()
    return [sha for sha in shas if not SIGNED_OFF_BY_RE.search(git("log", "-1", "--format=%B", sha))]


def main(argv: list[str]) -> int:
    if len(argv) != 2:
        print("usage: check-dco.py BASE HEAD", file=sys.stderr)
        return 2
    missing = unsigned_commits(*argv)
    if not missing:
        print("check-dco: every commit carries a Signed-off-by line")
        return 0
    print(f"check-dco: {len(missing)} commit(s) lack a Signed-off-by line (see CONTRIBUTING.md):", file=sys.stderr)
    for sha in missing:
        print(f"  {sha[:10]}  {git('log', '-1', '--format=%s', sha).strip()}", file=sys.stderr)
    print("add it with `git rebase --signoff <base>` and force-push the branch", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
