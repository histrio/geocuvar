#!/usr/bin/env python3
"""Delete changeset markdown files older than a retention window.

Deletion is based on the PublishDate front-matter field, not the file mtime,
so it also works in CI where a fresh git checkout resets every file's mtime
to "now" (which silently turned the old `find -mtime` cleanup into a no-op).
"""

import argparse
import datetime
import pathlib
import re
import sys

PUBLISH_DATE_RE = re.compile(
    r"^PublishDate:\s*(\d{4}-\d{2}-\d{2})", re.IGNORECASE | re.MULTILINE
)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--days",
        type=int,
        default=180,
        help="delete changesets published more than this many days ago (default: 180)",
    )
    parser.add_argument(
        "--dir",
        type=pathlib.Path,
        default=pathlib.Path("site/content/changesets"),
        help="directory with changeset .md files (default: site/content/changesets)",
    )
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="list files that would be deleted without deleting them",
    )
    args = parser.parse_args()

    if args.days < 1:
        parser.error("--days must be a positive integer")

    cutoff = (
        datetime.datetime.now(datetime.timezone.utc) - datetime.timedelta(days=args.days)
    ).date()
    cutoff_iso = cutoff.isoformat()

    removed = 0
    for path in sorted(args.dir.glob("*.md")):
        try:
            text = path.read_text(encoding="utf-8", errors="ignore")
        except OSError as exc:
            print(f"warning: cannot read {path}: {exc}", file=sys.stderr)
            continue

        match = PUBLISH_DATE_RE.search(text)
        if not match:
            continue  # undated files are left alone
        if match.group(1) >= cutoff_iso:
            continue

        removed += 1
        if args.dry_run:
            print(f"would delete {path}")
        else:
            path.unlink()

    verb = "Would delete" if args.dry_run else "Deleted"
    print(f"{verb} {removed} changeset(s) published before {cutoff_iso}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
