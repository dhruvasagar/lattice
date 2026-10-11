#!/usr/bin/env bash
# Publish the plugin API — the three crates an out-of-tree plugin builds
# against — to crates.io. See docs/dev/operations/releasing.md.
#
#   scripts/publish-plugin-api.sh             # what is local, what is published
#   scripts/publish-plugin-api.sh --dry-run   # every check, publish nothing
#   scripts/publish-plugin-api.sh --publish   # do it
#
# The crates are `lattice-plugin-sdk-derive`, `lattice-plugin-sdk` and
# `lattice-wit`. They carry their own version — the plugin ABI's, not the
# editor's — which `cargo xtask bump-plugin-api X.Y.Z` sets. `scripts/release.sh`
# deliberately does not touch them, and this script does not bump: it
# publishes the version that is already in the tree.
#
# PUBLISHING IS THE ONE STEP HERE THAT CANNOT BE TAKEN BACK. A version on
# crates.io can be yanked, never replaced; after `0.2.0` is up, a changed
# signature is `0.3.0`. So every check that could still say "not yet" runs
# before the first upload:
#
#   - on `main`, clean, and identical to `origin/main`. A published crate
#     records the commit it was built from, and that commit should be one
#     anybody can look at. It also means CI has seen what is being published.
#   - the three crates agree on a version, and it matches the WIT package's
#     (the guard test in `lattice-wit`).
#   - each crate packages and builds from its package alone (`--dry-run`).
#
# It is safe to re-run. A crate whose version is already on crates.io is
# skipped, so a run that died after the first upload — a network blip, an
# expired token — is finished by running it again.
#
# Order matters: the SDK depends on the derive crate at the same version and
# will not resolve until that is on the index. `cargo publish` waits for its
# own upload to appear there before returning, which is what lets these run
# back to back.
set -euo pipefail
# `cd` echoes the target when CDPATH is exported, which corrupts every
# command-substitution in this script that reads it.
unset CDPATH
cd "$(dirname "$0")/.." >/dev/null

# Dependency order. Do not sort.
CRATES=(lattice-plugin-sdk-derive lattice-plugin-sdk lattice-wit)

die() { printf '\npublish-plugin-api: %s\n\n' "$*" >&2; exit 1; }
say() { printf '\n=== %s\n' "$1"; }

# A crate's own `version = "X.Y.Z"`. These three state it literally —
# `version.workspace = true` would tie them to the editor's version, which is
# the thing they exist not to be tied to — so the first such line is the one.
crate_version() {
    grep -m1 -E '^version[[:space:]]*=[[:space:]]*"' "crates/$1/Cargo.toml" \
        | sed -E 's/.*"([^"]+)".*/\1/'
}

# Is `<crate> <version>` on crates.io? Echoes `yes`, `no`, or `unknown` (the
# registry could not be asked). crates.io rejects requests with no User-Agent.
published() {
    local status
    status="$(curl -s -o /dev/null -w '%{http_code}' -m 20 \
        -H 'User-Agent: lattice publish-plugin-api.sh' \
        "https://crates.io/api/v1/crates/$1/$2" 2>/dev/null)" || status=000
    case "$status" in
        200) echo yes ;;
        404) echo no ;;
        *)   echo unknown ;;
    esac
}

mode="status"
for arg in "$@"; do
    case "$arg" in
        --dry-run) mode="dry-run" ;;
        --publish) mode="publish" ;;
        *) die "unknown argument '$arg' (expected --dry-run or --publish)" ;;
    esac
done

# ------------------------------------------------------------------ versions
version=""
for crate in "${CRATES[@]}"; do
    [ -f "crates/$crate/Cargo.toml" ] || die "no crate at crates/$crate"
    v="$(crate_version "$crate")"
    [[ "$v" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
        || die "cannot read an X.Y.Z version out of crates/$crate/Cargo.toml (got '$v')"
    if [ -z "$version" ]; then
        version="$v"
    elif [ "$v" != "$version" ]; then
        die "the crates disagree on a version: $crate is $v, ${CRATES[0]} is $version.
They are bumped together — cargo xtask bump-plugin-api X.Y.Z"
    fi
done

say "plugin API $version"
todo=()
for crate in "${CRATES[@]}"; do
    case "$(published "$crate" "$version")" in
        yes)     printf '  %-28s %s  already on crates.io\n' "$crate" "$version" ;;
        no)      printf '  %-28s %s  not published\n' "$crate" "$version"; todo+=("$crate") ;;
        unknown) die "could not ask crates.io about $crate $version — no network?
Refusing to guess: publishing a version that is already there fails half way." ;;
    esac
done

if [ "${#todo[@]}" -eq 0 ]; then
    printf '\n  nothing to publish — %s is all there.\n\n' "$version"
    exit 0
fi

if [ "$mode" = "status" ]; then
    cat <<USAGE

  to publish: ${todo[*]}

  scripts/publish-plugin-api.sh --dry-run   # every check, publish nothing
  scripts/publish-plugin-api.sh --publish   # do it

USAGE
    exit 0
fi

if [ "$mode" = "dry-run" ]; then printf '\n(dry run — nothing will be published)\n'; fi

# ----------------------------------------------------------------- preflight
say "preflight"

branch="$(git rev-parse --abbrev-ref HEAD)"
[ "$branch" = "main" ] || die "on branch '$branch'; the plugin API is published from main.
Merge first: a published version cannot be replaced, and main is what CI has seen."

dirty="$(git status --porcelain)"
[ -z "$dirty" ] || die "working tree is not clean:
$dirty"

git fetch --quiet origin main || die "git fetch origin main failed"
[ "$(git rev-parse HEAD)" = "$(git rev-parse origin/main)" ] \
    || die "HEAD is not origin/main — pull or push first.
A published crate records its source commit; it should be one that is on origin."

echo "  on main, clean, in sync with origin/main ($(git rev-parse --short HEAD))"

# The crates' major.minor must equal the WIT package's, or `lattice-wit =
# "X.Y"` stops meaning ABI X.Y. The test also proves the bump reached every
# `.wit` file.
say "version guard"
cargo test --quiet -p lattice-wit --test the_crate_versions_track_the_wit_package_version \
    || die "the crate versions do not track the WIT package version — see the test output above"

# ------------------------------------------------------------------- publish
# One crate at a time, and each is dry-run immediately before its own upload
# rather than all up front: the SDK's dry-run cannot pass until the derive
# crate it depends on is really on the index.
for crate in "${todo[@]}"; do
    say "$crate $version"
    if [ "$crate" = "lattice-plugin-sdk" ] \
        && [ "$(published lattice-plugin-sdk-derive "$version")" != "yes" ]; then
        if [ "$mode" = "dry-run" ]; then
            echo "  skipped: depends on lattice-plugin-sdk-derive $version, which is not"
            echo "  on crates.io yet. A real run publishes that first, then checks this."
            continue
        fi
        die "lattice-plugin-sdk-derive $version is not on crates.io, and the SDK needs it"
    fi

    cargo publish -p "$crate" --dry-run \
        || die "$crate does not package cleanly — nothing was uploaded for it"
    if [ "$mode" = "publish" ]; then
        cargo publish -p "$crate" || die "publishing $crate failed.
Whatever was uploaded before it stays uploaded; fix the cause and re-run —
crates already on crates.io are skipped."
        echo "  published $crate $version"
    fi
done

if [ "$mode" = "dry-run" ]; then
    printf '\n  dry run complete. To publish: scripts/publish-plugin-api.sh --publish\n\n'
    exit 0
fi

minor="${version%.*}"
cat <<DONE

  plugin API $version is on crates.io.

  Out-of-tree plugins that declare \`lattice-wit\` are pinned to the previous
  generation until they move:

      lattice-wit = "$minor"

  and rebuild. Until then they do not load in an editor built from this tree.

DONE
