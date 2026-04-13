#!/usr/bin/env python3
"""
update_kanban.py — sync a KANBAN.md with live GitHub issues.

The script only touches content between sentinel HTML comments; everything
else (Done table, research backlog, roadmap, etc.) is left untouched.

──────────────────────────────────────────────────────────────────────────────
QUICK START
──────────────────────────────────────────────────────────────────────────────

  # auto-detect repo from git remote, KANBAN.md next to this script
  python3 update_kanban.py

  # explicit repo and file
  python3 update_kanban.py --repo owner/repo --kanban path/to/KANBAN.md

  # preview without writing
  python3 update_kanban.py --dry-run

  # generate a blank KANBAN.md with all required sentinels
  python3 update_kanban.py --init

Requirements: gh CLI (https://cli.github.com), authenticated with `gh auth login`.

──────────────────────────────────────────────────────────────────────────────
SENTINEL COMMENTS
──────────────────────────────────────────────────────────────────────────────

Add these comment pairs to your KANBAN.md.  The script replaces the content
between each pair on every run:

  ## 🔄 In Progress
  <!-- KANBAN:IN_PROGRESS -->
  …auto-generated…
  <!-- /KANBAN:IN_PROGRESS -->

  ## 📋 To Do
  <!-- KANBAN:TODO -->
  …auto-generated…
  <!-- /KANBAN:TODO -->

Run with --init to create a starter KANBAN.md with all sentinels in place.

──────────────────────────────────────────────────────────────────────────────
IN-PROGRESS DETECTION
──────────────────────────────────────────────────────────────────────────────

An issue is placed in "In Progress" when ANY of these is true:

  1. It carries a label whose name (case-insensitive) matches one of the
     patterns in IN_PROGRESS_LABELS  (default: in-progress, in progress,
     wip, doing).  Customise via --in-progress-label (repeatable).

  2. It has at least one assignee AND none of its labels match
     EXCLUDE_FROM_AUTO_PROGRESS  (default: backlog, research).
     Disable this behaviour with --no-assignee-heuristic.

──────────────────────────────────────────────────────────────────────────────
TO-DO GROUPING
──────────────────────────────────────────────────────────────────────────────

Issues are grouped in the To Do section using this priority order:

  1. GitHub milestone  — issues with a milestone appear under a "### <title>"
     heading derived from that milestone name.  Use --group-by=milestone
     (default).

  2. Label             — use --group-by=label to group by the first label
     that is not an in-progress or exclude label.

  3. None              — use --group-by=none; all issues appear in one flat
     table with no sub-headings.

In all modes, issues with no group fall into an "Unsorted" section at the end.
"""

import argparse
import json
import re
import subprocess
import sys
from collections import defaultdict
from datetime import date
from pathlib import Path

# ── Defaults (all overridable via CLI flags) ──────────────────────────────────

DEFAULT_KANBAN = Path(__file__).parent / "KANBAN.md"

# Label names (lowercase) that mark an issue as "in progress"
DEFAULT_IN_PROGRESS_LABELS: set[str] = {"in-progress", "in progress", "wip", "doing"}

# Label names (lowercase) that suppress the assignee → in-progress heuristic
DEFAULT_EXCLUDE_LABELS: set[str] = {"backlog", "research"}

BLANK_KANBAN_TEMPLATE = """\
# {repo} Kanban

---

## ✅ Done

| Item | Notes |
|------|-------|
| _(move closed issues here manually)_ | |

---

## 🔄 In Progress

<!-- KANBAN:IN_PROGRESS -->
_No issues currently in progress._
<!-- /KANBAN:IN_PROGRESS -->

---

## 📋 To Do

<!-- KANBAN:TODO -->
_No open issues._
<!-- /KANBAN:TODO -->

---

*Updated: {today}*
"""

# ── gh CLI wrapper ────────────────────────────────────────────────────────────

def gh_run(*args: str) -> str:
    cmd = ["gh", *args]
    try:
        result = subprocess.run(cmd, capture_output=True, text=True, check=True)
        return result.stdout
    except FileNotFoundError:
        sys.exit("Error: 'gh' CLI not found.  Install it: https://cli.github.com")
    except subprocess.CalledProcessError as e:
        sys.exit(f"Error running {' '.join(cmd)}:\n{e.stderr.strip()}")


def gh_json(*args: str) -> list[dict] | dict:
    return json.loads(gh_run(*args))


def detect_repo() -> str | None:
    """Try to read owner/repo from the current git remote."""
    try:
        out = subprocess.run(
            ["git", "remote", "get-url", "origin"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        # SSH:   git@github.com:owner/repo.git
        # HTTPS: https://github.com/owner/repo.git
        m = re.search(r"github\.com[:/](.+?)(?:\.git)?$", out)
        if m:
            return m.group(1)
    except (FileNotFoundError, subprocess.CalledProcessError):
        pass
    return None

# ── Issue helpers ─────────────────────────────────────────────────────────────

def label_names(issue: dict) -> set[str]:
    return {lbl["name"].lower() for lbl in issue.get("labels", [])}


def is_in_progress(issue: dict, in_progress_labels: set[str],
                   exclude_labels: set[str], assignee_heuristic: bool) -> bool:
    lbls = label_names(issue)
    if lbls & in_progress_labels:
        return True
    if assignee_heuristic and issue.get("assignees") and not (lbls & exclude_labels):
        return True
    return False


def milestone_of(issue: dict) -> str:
    m = issue.get("milestone")
    return m.get("title", "") if isinstance(m, dict) else ""


def first_grouping_label(issue: dict, skip: set[str]) -> str:
    for lbl in issue.get("labels", []):
        if lbl["name"].lower() not in skip:
            return lbl["name"]
    return ""


def format_labels(issue: dict, skip: set[str]) -> str:
    names = [lbl["name"] for lbl in issue.get("labels", [])
             if lbl["name"].lower() not in skip]
    return " ".join(f"`{n}`" for n in names) if names else ""


def issue_note(issue: dict) -> str:
    """Extract a short note from the issue body (first plain-text sentence)."""
    body = (issue.get("body") or "").strip()
    for line in body.splitlines():
        line = line.strip()
        if line and not line.startswith("#") and not line.startswith("|") \
                and not line.startswith("- [") and len(line) < 140:
            return line.lstrip("> ").split(".")[0]
    return ""


def issue_row(issue: dict, skip_labels: set[str]) -> str:
    number = issue["number"]
    title  = issue["title"]
    url    = issue.get("url", f"https://github.com/issues/{number}")
    labels = format_labels(issue, skip_labels)
    note   = issue_note(issue)
    return f"| [#{number}]({url}) {title} | {labels} | {note} |"

# ── Section builders ──────────────────────────────────────────────────────────

TABLE_HEADER = "| Item | Labels | Notes |\n|------|--------|-------|"


def build_in_progress(issues: list[dict], skip_labels: set[str]) -> str:
    if not issues:
        return "_No issues currently in progress._\n"
    rows = "\n".join(issue_row(i, skip_labels) for i in issues)
    return TABLE_HEADER + "\n" + rows + "\n"


def build_todo(issues: list[dict], group_by: str, skip_labels: set[str]) -> str:
    if not issues:
        return "_No open issues._\n"

    # ── group issues ──────────────────────────────────────────────────────────
    buckets: dict[str, list[dict]] = defaultdict(list)
    if group_by == "none":
        buckets[""] = list(issues)
    else:
        for issue in issues:
            if group_by == "milestone":
                key = milestone_of(issue) or ""
            else:  # label
                key = first_grouping_label(issue, skip_labels)
            buckets[key].append(issue)

    # ── render ────────────────────────────────────────────────────────────────
    parts: list[str] = []

    named   = sorted(k for k in buckets if k)
    unnamed = [k for k in buckets if not k]

    for key in named + unnamed:
        group = buckets[key]
        if group_by != "none":
            heading = f"### {key}" if key else "### Unsorted"
            parts += [heading, ""]
        parts += [TABLE_HEADER]
        parts += [issue_row(i, skip_labels) for i in group]
        parts.append("")

    return "\n".join(parts)

# ── KANBAN.md rewriting ───────────────────────────────────────────────────────

def replace_between(text: str, start_tag: str, end_tag: str, replacement: str) -> str:
    pattern = re.compile(
        rf"({re.escape(start_tag)}\n).*?(\n{re.escape(end_tag)})",
        re.DOTALL,
    )
    if not pattern.search(text):
        print(f"  Warning: sentinel '{start_tag}' not found — section not updated.")
        return text
    return pattern.sub(rf"\g<1>{replacement}\g<2>", text)


def update_datestamp(text: str) -> str:
    today = date.today().isoformat()
    new_stamp = f"*Updated: {today}*"
    updated, n = re.subn(r"\*Updated:.*?\*", new_stamp, text)
    return updated if n else text.rstrip() + f"\n\n{new_stamp}\n"

# ── CLI ───────────────────────────────────────────────────────────────────────

def build_parser() -> argparse.ArgumentParser:
    p = argparse.ArgumentParser(
        description=__doc__,
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument(
        "--repo", default=None,
        help="GitHub owner/repo.  Defaults to the remote of the current git repo.",
    )
    p.add_argument(
        "--kanban", default=DEFAULT_KANBAN, type=Path,
        help=f"Path to KANBAN.md  (default: {DEFAULT_KANBAN})",
    )
    p.add_argument(
        "--group-by", choices=["milestone", "label", "none"], default="milestone",
        help="How to group To Do issues  (default: milestone)",
    )
    p.add_argument(
        "--in-progress-label", dest="in_progress_labels",
        action="append", metavar="LABEL",
        help="Label name that marks an issue as in-progress  (repeatable; "
             "default: in-progress, wip, doing, 'in progress')",
    )
    p.add_argument(
        "--no-assignee-heuristic", dest="assignee_heuristic",
        action="store_false", default=True,
        help="Disable: assigned issues without exclude-labels → in-progress",
    )
    p.add_argument(
        "--exclude-label", dest="exclude_labels",
        action="append", metavar="LABEL",
        help="Label that suppresses the assignee heuristic  (repeatable; "
             "default: backlog, research)",
    )
    p.add_argument(
        "--limit", type=int, default=200,
        help="Maximum number of issues to fetch  (default: 200)",
    )
    p.add_argument(
        "--dry-run", action="store_true",
        help="Print the updated KANBAN.md to stdout instead of writing it",
    )
    p.add_argument(
        "--init", action="store_true",
        help="Create a blank KANBAN.md with sentinel comments and exit",
    )
    return p


def main() -> None:
    args = build_parser().parse_args()

    # ── Resolve repo ──────────────────────────────────────────────────────────
    repo = args.repo or detect_repo()
    if not repo:
        sys.exit(
            "Error: could not detect repo from git remote.\n"
            "Pass --repo owner/repo explicitly."
        )

    # ── Resolve label sets ────────────────────────────────────────────────────
    in_progress_labels = (
        {l.lower() for l in args.in_progress_labels}
        if args.in_progress_labels
        else DEFAULT_IN_PROGRESS_LABELS
    )
    exclude_labels = (
        {l.lower() for l in args.exclude_labels}
        if args.exclude_labels
        else DEFAULT_EXCLUDE_LABELS
    )
    skip_labels = in_progress_labels | exclude_labels

    # ── --init ────────────────────────────────────────────────────────────────
    if args.init:
        kanban_path: Path = args.kanban
        if kanban_path.exists():
            sys.exit(f"Error: {kanban_path} already exists.  Remove it first.")
        kanban_path.write_text(
            BLANK_KANBAN_TEMPLATE.format(repo=repo, today=date.today().isoformat())
        )
        print(f"Created {kanban_path}  (repo: {repo})")
        return

    # ── Fetch issues ──────────────────────────────────────────────────────────
    print(f"Fetching open issues from {repo} …")
    issues: list[dict] = gh_json(
        "issue", "list",
        "--repo", repo,
        "--state", "open",
        "--limit", str(args.limit),
        "--json", "number,title,url,labels,milestone,assignees,body",
    )
    print(f"  {len(issues)} open issue(s) found.")

    in_prog = [i for i in issues
               if is_in_progress(i, in_progress_labels, exclude_labels,
                                  args.assignee_heuristic)]
    todo    = [i for i in issues if i not in in_prog]

    print(f"  In progress : {len(in_prog)}")
    print(f"  To do       : {len(todo)}  (grouped by: {args.group_by})")

    # ── Read KANBAN.md ────────────────────────────────────────────────────────
    kanban_path = args.kanban
    if not kanban_path.exists():
        sys.exit(
            f"Error: {kanban_path} not found.\n"
            f"Run with --init to create a starter file, or pass --kanban <path>."
        )

    text = kanban_path.read_text()

    # ── Apply replacements ────────────────────────────────────────────────────
    text = replace_between(
        text,
        "<!-- KANBAN:IN_PROGRESS -->", "<!-- /KANBAN:IN_PROGRESS -->",
        build_in_progress(in_prog, skip_labels),
    )
    text = replace_between(
        text,
        "<!-- KANBAN:TODO -->", "<!-- /KANBAN:TODO -->",
        build_todo(todo, args.group_by, skip_labels),
    )
    text = update_datestamp(text)

    # ── Write / print ─────────────────────────────────────────────────────────
    if args.dry_run:
        print("\n" + "─" * 60 + "\n")
        print(text)
    else:
        kanban_path.write_text(text)
        print(f"  ✓ {kanban_path} updated.")


if __name__ == "__main__":
    main()
