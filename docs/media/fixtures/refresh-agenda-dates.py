#!/usr/bin/env python3
"""Shift `demo-agenda.org`'s dates onto today, so the agenda shot has a today.

The fixture is a committed file, so its dates go stale the moment it is
written. Screenshot 4 needs a populated "today" section — an agenda whose
today section is empty is not the shot — and recapture happens whenever the
UI changes, so this has to be repeatable rather than a one-time hand edit.

Every timestamp in the file is shifted by (today - `#+ANCHOR:`), and the
anchor is then reset to today. Weekday names are recomputed, so `<... Wed>`
stays correct. Running it twice in one day is a no-op.

    python3 docs/media/fixtures/refresh-agenda-dates.py [--check]

`--check` reports whether a shift is needed without writing, for use in a
pre-capture checklist.
"""

import argparse
import datetime as dt
import pathlib
import re
import sys

FIXTURE = pathlib.Path(__file__).with_name("demo-agenda.org")
ANCHOR_RE = re.compile(r"^#\+ANCHOR:\s*(\d{4}-\d{2}-\d{2})\s*$", re.MULTILINE)
# Active <...> and inactive [...] timestamps alike: date, optional weekday,
# optional time. The weekday is recomputed rather than carried over.
STAMP_RE = re.compile(
    r"(?P<open>[<\[])"
    r"(?P<date>\d{4}-\d{2}-\d{2})"
    r"(?:\s+[A-Za-z]{3})?"
    r"(?P<rest>(?:\s+\d{2}:\d{2}(?:-\d{2}:\d{2})?)?)"
    r"(?P<close>[>\]])"
)


def shift(text: str, delta: dt.timedelta) -> str:
    def one(m: re.Match) -> str:
        date = dt.date.fromisoformat(m.group("date")) + delta
        day = date.strftime("%a")
        return f"{m.group('open')}{date.isoformat()} {day}{m.group('rest')}{m.group('close')}"

    return STAMP_RE.sub(one, text)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="report whether a shift is needed; do not write",
    )
    args = parser.parse_args()

    text = FIXTURE.read_text()
    match = ANCHOR_RE.search(text)
    if not match:
        print(f"{FIXTURE.name}: no `#+ANCHOR: YYYY-MM-DD` line", file=sys.stderr)
        return 1

    anchor = dt.date.fromisoformat(match.group(1))
    today = dt.date.today()
    delta = today - anchor

    if not delta:
        print(f"{FIXTURE.name}: already anchored on {today} — nothing to do")
        return 0

    if args.check:
        print(
            f"{FIXTURE.name}: anchored on {anchor}, {delta.days:+d} days from "
            f"today — run without --check before capturing"
        )
        return 1

    shifted = ANCHOR_RE.sub(f"#+ANCHOR: {today.isoformat()}", shift(text, delta))
    FIXTURE.write_text(shifted)
    print(f"{FIXTURE.name}: shifted {delta.days:+d} days, anchored on {today}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
