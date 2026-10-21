#!/usr/bin/env bash
# Cut a release: bump the one workspace version, refresh the lock, commit,
# tag. See docs/dev/architecture/release-pipeline.md for what the tag then
# triggers.
#
#   scripts/release.sh                  # last release + what each bump gives
#   scripts/release.sh patch            # 0.9.0 -> 0.9.1
#   scripts/release.sh minor            # 0.9.0 -> 0.10.0
#   scripts/release.sh major            # 0.9.0 -> 1.0.0
#   scripts/release.sh 0.11.0           # an explicit version
#   scripts/release.sh minor --dry-run  # print every step, change nothing
#
# Bumps count from the LAST RELEASE — the highest `vX.Y.Z` tag — not from
# `Cargo.toml`. The two differ when `Cargo.toml` was bumped ahead of a release
# (a plugin's `min_lattice` naming the editor before it ships), and counting
# from `Cargo.toml` then skipped a version: 0.9.9 was released, `Cargo.toml`
# said 0.9.10, and `patch` cut 0.9.11. Now `patch` after 0.9.9 is 0.9.10, and
# a `Cargo.toml` already there is released as is.
#
# 38 of the 41 crates are `version.workspace = true`, so a bump is ONE line in
# Cargo.toml plus the workspace entries in Cargo.lock. The `prepare` job in
# .github/workflows/release.yml hard-fails when the tag and that line
# disagree; this script's job is to make them agree and never to guess.
#
# The three exceptions are the PUBLISHED crates — `lattice-wit`,
# `lattice-plugin-sdk`, `lattice-plugin-sdk-derive` — which carry their own
# versions and are deliberately NOT bumped here. Their compatibility story is
# the plugin ABI's, not the editor's: an editor patch release must not push a
# new version at every plugin author for a crate that did not change. Releasing
# them is a separate act; see docs/dev/operations/releasing.md.
#
# It deliberately STOPS at the local tag and prints the push line. Pushing
# the tag fires the release pipeline, which publishes public artefacts —
# that stays a separate, deliberate act.
#
# It also refuses to tag a version CHANGELOG.md has no written section for.
# A written `## Unreleased` section counts: it is retitled for the version.
# The changelog is theme-grouped prose, not a commit list; nothing here can
# author it, and the release body is built from it, so a missing section
# would ship an empty release page.
set -euo pipefail
# `cd` echoes the target when CDPATH is exported, which corrupts every
# command-substitution in this script that reads it.
unset CDPATH
cd "$(dirname "$0")/.." >/dev/null

STUB_MARKER="<!-- write the release notes here, then re-run scripts/release.sh -->"

die() { printf '\nrelease: %s\n\n' "$*" >&2; exit 1; }
say() { printf '\n=== %s\n' "$1"; }

# The SAME expression release.yml's `prepare` job uses to read the version.
# If the two ever disagree the tag guard fails mid-release, so they are
# deliberately identical.
read_version() {
    grep -m1 -E '^version[[:space:]]*=' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/'
}

# The `## X.Y.Z` section of CHANGELOG.md, heading included, up to the next
# `## `. Also used verbatim as the annotated tag's message.
extract_section() {
    awk -v ver="$1" '
        BEGIN { gsub(/\./, "\\.", ver) }
        $0 ~ "^## " ver "([ \t]|$)" { inside = 1; print; next }
        inside && /^## / { exit }
        inside { print }
    ' CHANGELOG.md
}

current="$(read_version)"
[[ "$current" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || die "cannot parse workspace version '$current' out of Cargo.toml"

# The highest released version: the greatest `vX.Y.Z` tag. Empty before the
# first release, when bumps count from `Cargo.toml` instead.
last_release() {
    git tag --list 'v[0-9]*.[0-9]*.[0-9]*' \
        | sed 's/^v//' \
        | grep -E '^[0-9]+\.[0-9]+\.[0-9]+$' \
        | sort -V | tail -1 || true
}
# Whether version $1 sorts strictly before $2.
version_lt() {
    [ "$1" != "$2" ] && [ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | head -1)" = "$1" ]
}

released="$(last_release)"
base="${released:-$current}"
[[ "$base" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] || die "cannot parse version '$base'"
base_major="${BASH_REMATCH[1]}"
base_minor="${BASH_REMATCH[2]}"
base_patch="${BASH_REMATCH[3]}"

if [ "$#" -eq 0 ]; then
    pending=""
    if [ -n "$released" ] && version_lt "$released" "$current"; then
        pending="   (ahead of the last release — scripts/release.sh $current releases it as is)"
    fi
    cat <<USAGE

  released  v${released:-none}
  Cargo     v$current$pending

  patch     v$base_major.$base_minor.$((base_patch + 1))
  minor     v$base_major.$((base_minor + 1)).0
  major     v$((base_major + 1)).0.0

  scripts/release.sh <patch|minor|major|X.Y.Z> [--dry-run]

USAGE
    exit 0
fi

bump=""
dry_run=0
for arg in "$@"; do
    case "$arg" in
        --dry-run)            dry_run=1 ;;
        patch|minor|major)    bump="$arg" ;;
        [0-9]*.[0-9]*.[0-9]*) bump="$arg" ;;
        *) die "unknown argument '$arg' (expected patch|minor|major|X.Y.Z|--dry-run)" ;;
    esac
done
[ -n "$bump" ] || die "no bump given (patch|minor|major|X.Y.Z)"

case "$bump" in
    patch) next="$base_major.$base_minor.$((base_patch + 1))" ;;
    minor) next="$base_major.$((base_minor + 1)).0" ;;
    major) next="$((base_major + 1)).0.0" ;;
    *)     next="$bump" ;;
esac
[[ "$next" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "'$next' is not an X.Y.Z version"

# Never backwards, and never a version already released.
if [ -n "$released" ] && ! version_lt "$released" "$next"; then
    die "v$next is not newer than the last release, v$released"
fi
# `Cargo.toml` already ahead of the bump asked for: releasing would LOWER it.
if version_lt "$next" "$current"; then
    die "Cargo.toml is already at $current, ahead of the $bump bump (v$next) from v${released:-$current}.
  Release it as is:   scripts/release.sh $current
  or go past it:      scripts/release.sh <X.Y.Z newer than $current>"
fi

# The version may already be the one being released: `Cargo.toml` is bumped
# ahead of the release when something in the tree needs to say which editor it
# targets before that editor ships (a plugin's `min_lattice`). Releasing it is
# then tagging what is there — no bump. Whether the tag is free is checked in
# preflight like any other.
prebumped=0
[ "$next" != "$current" ] || prebumped=1

tag="v$next"
if [ "$dry_run" -eq 1 ]; then printf '\n(dry run — nothing will be changed)\n'; fi

# ---------------------------------------------------------------- preflight
say "preflight (v$current -> $tag)"

branch="$(git rev-parse --abbrev-ref HEAD)"
[ "$branch" = "main" ] || die "on branch '$branch'; releases are cut from main"

# CHANGELOG.md is allowed to be dirty. Writing the release notes and cutting
# the release is one motion, and both land in the release commit together.
dirty="$(git status --porcelain -- . ':!CHANGELOG.md')"
[ -z "$dirty" ] || die "working tree is not clean (only CHANGELOG.md may be modified):
$dirty"

git fetch --quiet --tags origin main || die "git fetch origin main failed"
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] \
    || die "HEAD is not origin/main — pull or push first"

if git rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
    die "tag $tag already exists locally"
fi
[ -z "$(git ls-remote --tags origin "refs/tags/$tag")" ] \
    || die "tag $tag already exists on origin"

# Tags were just fetched: re-check against the newest release anyone has cut,
# not only the ones this clone knew about when the version was computed.
released_now="$(last_release)"
if [ -n "$released_now" ] && ! version_lt "$released_now" "$next"; then
    die "v$next is not newer than the last release on origin, v$released_now"
fi

echo "  on main, in sync with origin/main, $tag is free"

# ---------------------------------------------------------------- changelog
say "changelog"

# Notes written as the work landed sit under `## Unreleased`. That section IS
# the release's notes, so it is promoted — retitled with the version and the
# date — rather than answered with an empty stub beside it. Without this the
# script reported "no section" for a changelog that had one, and the author
# was left to move the text by hand.
if ! extract_section "$next" | grep -q . \
    && extract_section Unreleased | tail -n +2 | grep -q '[^[:space:]]'; then
    if [ "$dry_run" -eq 1 ]; then
        die "CHANGELOG.md has no '## $next' section, but '## Unreleased' is written (a real run would retitle it '## $next')"
    fi
    tmp="$(mktemp)"
    awk -v ver="$next" -v day="$(date +%F)" '
        !done && /^## Unreleased[ \t]*$/ { print "## " ver " — " day; done = 1; next }
        { print }
    ' CHANGELOG.md > "$tmp"
    cat "$tmp" > CHANGELOG.md
    rm -f "$tmp"
    echo "  '## Unreleased' retitled '## $next — $(date +%F)'"
fi

if ! extract_section "$next" | grep -q .; then
    [ "$dry_run" -eq 0 ] \
        || die "CHANGELOG.md has no '## $next' section (a real run would insert a stub)"
    tmp="$(mktemp)"
    awk -v ver="$next" -v day="$(date +%F)" -v marker="$STUB_MARKER" '
        { print }
        !inserted && $0 == "# Changelog" {
            print ""
            print "## " ver " — " day
            print ""
            print marker
            inserted = 1
        }
    ' CHANGELOG.md > "$tmp"
    cat "$tmp" > CHANGELOG.md
    rm -f "$tmp"
    die "CHANGELOG.md had no '## $next' section — a dated stub is now in place.
Write the notes (theme-grouped prose; see the 0.9.0 entry), then re-run:

    scripts/release.sh $bump"
fi

section="$(extract_section "$next")"
case "$section" in
    *"$STUB_MARKER"*) die "the '## $next' section of CHANGELOG.md is still the stub — write it first" ;;
esac
body="$(printf '%s\n' "$section" | tail -n +2 | tr -d '[:space:]')"
[ -n "$body" ] || die "the '## $next' section of CHANGELOG.md is empty"
echo "  '## $next' section present ($(printf '%s\n' "$section" | wc -l | tr -d ' ') lines) — it becomes the tag message and the GitHub release body"

# --------------------------------------------------------------------- bump
say "bump"

if [ "$prebumped" -eq 1 ]; then
    echo "  Cargo.toml is already $next (bumped ahead of the release) — nothing to bump"
elif [ "$dry_run" -eq 1 ]; then
    echo "  would set Cargo.toml version = \"$next\""
    echo "  would run cargo update --workspace"
else
    tmp="$(mktemp)"
    awk -v ver="$next" '
        !done && /^version[[:space:]]*=/ { print "version = \"" ver "\""; done = 1; next }
        { print }
    ' Cargo.toml > "$tmp"
    cat "$tmp" > Cargo.toml
    rm -f "$tmp"
    now="$(read_version)"
    [ "$now" = "$next" ] || die "Cargo.toml still reads '$now' after the rewrite"
    echo "  Cargo.toml: $current -> $next"

    # Re-resolve the workspace members in Cargo.lock. `--workspace` touches
    # ONLY members; external dependency versions are left exactly as they are.
    cargo update --workspace --quiet || die "cargo update --workspace failed"
    echo "  Cargo.lock refreshed"
fi

# ---------------------------------------------------------- commit and tag
say "commit and tag"

if [ "$dry_run" -eq 1 ]; then
    echo "  would git add Cargo.toml Cargo.lock CHANGELOG.md"
    echo "  would git commit -m 'chore(release): $tag'"
    echo "  would git tag -a $tag  (message = the '## $next' changelog section)"
    printf '\n(dry run — nothing was changed)\n\n'
    exit 0
fi

# Explicit paths, never `git add -A`: docs/dev/notes/todo.org is a scratch
# file that must never be committed.
git add Cargo.toml Cargo.lock CHANGELOG.md
git commit --quiet -m "chore(release): $tag"
# --cleanup=whitespace, NOT the default `strip`: the section is markdown,
# and `strip` deletes every `#`-leading line as a comment — which is the
# heading and every `### Theme` subheading in it.
printf '%s\n' "$section" | git tag -a "$tag" --cleanup=whitespace -F -
echo "  committed, and tagged $tag"

cat <<NEXT

=== next

  Nothing has been pushed. Review:

      git show --stat HEAD
      git show $tag

  Fire the release pipeline:

      git push origin main && git push origin $tag

  Or undo:

      git tag -d $tag && git reset --hard HEAD~1

NEXT
