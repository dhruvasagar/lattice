#!/usr/bin/env bash
# Cut a release: bump the one workspace version, refresh the lock, commit,
# tag. See docs/dev/architecture/release-pipeline.md for what the tag then
# triggers.
#
#   scripts/release.sh                  # current version + what each bump gives
#   scripts/release.sh patch            # 0.9.0 -> 0.9.1
#   scripts/release.sh minor            # 0.9.0 -> 0.10.0
#   scripts/release.sh major            # 0.9.0 -> 1.0.0
#   scripts/release.sh 0.11.0           # an explicit version
#   scripts/release.sh minor --dry-run  # print every step, change nothing
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
[[ "$current" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] \
    || die "cannot parse workspace version '$current' out of Cargo.toml"
cur_major="${BASH_REMATCH[1]}"
cur_minor="${BASH_REMATCH[2]}"
cur_patch="${BASH_REMATCH[3]}"

if [ "$#" -eq 0 ]; then
    cat <<USAGE

  current   v$current

  patch     v$cur_major.$cur_minor.$((cur_patch + 1))
  minor     v$cur_major.$((cur_minor + 1)).0
  major     v$((cur_major + 1)).0.0

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
    patch) next="$cur_major.$cur_minor.$((cur_patch + 1))" ;;
    minor) next="$cur_major.$((cur_minor + 1)).0" ;;
    major) next="$((cur_major + 1)).0.0" ;;
    *)     next="$bump" ;;
esac
[[ "$next" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "'$next' is not an X.Y.Z version"
[ "$next" != "$current" ] || die "already at v$current"

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

echo "  on main, in sync with origin/main, $tag is free"

# ---------------------------------------------------------------- changelog
say "changelog"

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

if [ "$dry_run" -eq 1 ]; then
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
