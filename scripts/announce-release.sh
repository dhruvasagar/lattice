#!/usr/bin/env bash
# Announce a published release in the Discord server's announcement channel.
#
#   scripts/announce-release.sh <title> <url> <notes-file>
#
# Used by every release path, after the release EXISTS: lattice's
# `release.yml` (after the GitHub Release is created), `plugin-release.yml`
# (after `gh release create`, so org and any plugin calling it), and
# `scripts/publish-plugin-api.sh --publish` (after the last crate uploads).
# Announcing at tag time instead would have announced v0.10.1, which was
# tagged and never published.
#
# The webhook comes from `DISCORD_ANNOUNCE_WEBHOOK`. Unset is not an error:
# a plugin calling `plugin-release.yml` without the secret, or a local run
# without the variable, says it skipped and exits 0. A failed post warns and
# exits 0 too. The release is already out by the time this runs, so failing
# its job would mark a published release red and invite a re-run that cannot
# re-create it. Post again by hand with this script.
#
# The notes are the release's own (its CHANGELOG section, or a plugin tag's
# message), sent as an embed. An embed description holds 4096 characters, so a
# long section is cut at a line boundary and ends with a link to the full
# notes. Markdown headings become bold lines, since Discord does not render
# headings inside an embed.
set -euo pipefail

if [ $# -ne 3 ]; then
	echo "usage: $0 <title> <url> <notes-file>" >&2
	exit 2
fi
title="$1" url="$2" notes="$3"

warn() {
	if [ -n "${GITHUB_ACTIONS:-}" ]; then
		echo "::warning::$*"
	else
		echo "announce-release: $*" >&2
	fi
}

if [ -z "${DISCORD_ANNOUNCE_WEBHOOK:-}" ]; then
	echo "announce-release: DISCORD_ANNOUNCE_WEBHOOK is not set; not announcing $title"
	exit 0
fi
if [ ! -r "$notes" ]; then
	warn "notes file $notes is missing; not announcing $title"
	exit 0
fi

payload="$(python3 -I - "$title" "$url" "$notes" <<'PY'
import json, re, sys

title, url, path = sys.argv[1:4]
with open(path, encoding="utf-8") as f:
    lines = f.read().strip().splitlines()

# `### Fixed` -> `**Fixed**`: embeds show headings as literal `#`s.
lines = [re.sub(r"^#{1,6}\s+(.*)$", r"**\1**", l) for l in lines]

LIMIT = 4096
more = f"\n\n… [full release notes]({url})"
body = "\n".join(lines)
if len(body) > LIMIT:
    kept = []
    size = 0
    for l in lines:
        if size + len(l) + 1 + len(more) > LIMIT:
            break
        kept.append(l)
        size += len(l) + 1
    body = "\n".join(kept).rstrip() + more

print(json.dumps({
    "embeds": [{
        "title": title,
        "url": url,
        "description": body,
        "color": 0x5865F2,
    }],
    "allowed_mentions": {"parse": []},
}))
PY
)"

if curl -fsS --retry 3 --max-time 30 \
	-H 'Content-Type: application/json' \
	--data-binary "$payload" \
	"$DISCORD_ANNOUNCE_WEBHOOK" >/dev/null; then
	echo "announce-release: announced $title"
else
	warn "posting the Discord announcement for $title failed; re-run scripts/announce-release.sh by hand"
fi
