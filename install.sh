#!/bin/sh
# Lattice installer. Resolves a release archive for this platform, verifies it
# against the release's SHA256SUMS, and installs a relocatable prefix.
#
#   curl -fsSL https://raw.githubusercontent.com/dhruvasagar/lattice/main/install.sh | sh
#   ... | sh -s -- --prefix /usr/local --gui --version v0.9.0
#
# The archive layout matters: `bin/lattice` discovers its bundled plugins
# through `../share/lattice/plugins`, so both trees are installed together.
set -eu

REPO="dhruvasagar/lattice"
PREFIX="${LATTICE_PREFIX:-$HOME/.local}"
VERSION="${LATTICE_VERSION:-}"
FLAVOUR="lattice"

die() { printf 'install.sh: %s\n' "$1" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }

while [ $# -gt 0 ]; do
	case "$1" in
		--prefix)
			[ $# -ge 2 ] || die "--prefix needs a directory"
			case "$2" in
				-*) die "--prefix needs a directory" ;;
			esac
			PREFIX="$2"; shift 2 ;;
		--version)
			[ $# -ge 2 ] || die "--version needs a tag"
			case "$2" in
				-*) die "--version needs a tag" ;;
			esac
			VERSION="$2"; shift 2 ;;
		--gui) FLAVOUR="lattice-gui"; shift ;;
		-h|--help)
			cat <<'EOF'
usage: install.sh [--prefix DIR] [--version TAG] [--gui]

  --prefix DIR    install root (default: ~/.local)
  --version TAG   release tag, e.g. v0.9.0 (default: latest)
  --gui           install the GPU-rendered build instead of the terminal build
EOF
			exit 0 ;;
		*) die "unknown option: $1" ;;
	esac
done

have curl || die "curl is required"
have tar || die "tar is required"

case "$(uname -s)" in
	Darwin) os="macos" ;;
	Linux) os="linux" ;;
	*) die "unsupported OS $(uname -s) — see https://github.com/$REPO/releases for Windows archives" ;;
esac

case "$(uname -m)" in
	x86_64|amd64) arch="x86_64" ;;
	arm64|aarch64) arch="aarch64" ;;
	*) die "unsupported architecture $(uname -m)" ;;
esac

if [ -z "$VERSION" ]; then
	VERSION="$(curl -fsSL "https://api.github.com/repos/$REPO/releases/latest" \
		| sed -n 's/.*"tag_name": *"\([^"]*\)".*/\1/p' | head -n 1)"
	[ -n "$VERSION" ] || die "could not resolve the latest release tag; pass --version"
fi

ver="${VERSION#v}"
archive="$FLAVOUR-$ver-$arch-$os.tar.xz"
base="https://github.com/$REPO/releases/download/$VERSION"

tmp="$(mktemp -d)"
cleanup() {
	rc=$?
	rm -rf "$tmp"
	rm -rf "$PREFIX/share/lattice/.plugins.new.$$"
	if [ -d "$PREFIX/share/lattice/plugins.old.$$" ]; then
		if [ -d "$PREFIX/share/lattice/plugins" ]; then
			rm -rf "$PREFIX/share/lattice/plugins.old.$$"
		else
			mv "$PREFIX/share/lattice/plugins.old.$$" "$PREFIX/share/lattice/plugins"
		fi
	fi
	rm -f "$PREFIX/bin/lattice.new.$$"
	exit "$rc"
}
trap cleanup EXIT INT TERM

printf 'Downloading %s (%s)…\n' "$archive" "$VERSION"
curl -fSL --progress-bar -o "$tmp/$archive" "$base/$archive" \
	|| die "no such archive: $base/$archive
The GUI build is best-effort on ARM Linux; try without --gui, or see
https://github.com/$REPO/releases"
curl -fsSL -o "$tmp/SHA256SUMS" "$base/SHA256SUMS" || die "could not fetch SHA256SUMS"

printf 'Verifying checksum…\n'
if have sha256sum; then sum_cmd="sha256sum"
elif have shasum; then sum_cmd="shasum -a 256"
else die "neither sha256sum nor shasum is available"
fi
want="$(grep " $archive\$" "$tmp/SHA256SUMS" | awk '{print $1}')"
[ -n "$want" ] || die "$archive is not listed in SHA256SUMS"
got="$(cd "$tmp" && $sum_cmd "$archive" | awk '{print $1}')"
[ "$want" = "$got" ] || die "checksum mismatch for $archive
  expected $want
  got      $got"

printf 'Installing to %s…\n' "$PREFIX"
tar xJf "$tmp/$archive" -C "$tmp"
root="$tmp/$FLAVOUR-$ver-$arch-$os"
[ -x "$root/bin/lattice" ] || die "archive has no bin/lattice — layout changed?"
[ -d "$root/share/lattice/plugins" ] || die "archive carries no bundled plugins — refusing to install a crippled editor"

mkdir -p "$PREFIX/bin" "$PREFIX/share/lattice"

stage="$PREFIX/share/lattice/.plugins.new.$$"
rm -rf "$stage"
cp -R "$root/share/lattice/plugins" "$stage"
if [ -d "$PREFIX/share/lattice/plugins" ]; then
	rm -rf "$PREFIX/share/lattice/plugins.old.$$"
	mv "$PREFIX/share/lattice/plugins" "$PREFIX/share/lattice/plugins.old.$$"
fi
mv "$stage" "$PREFIX/share/lattice/plugins"
rm -rf "$PREFIX/share/lattice/plugins.old.$$"

cp "$root/bin/lattice" "$PREFIX/bin/lattice.new.$$"
chmod +x "$PREFIX/bin/lattice.new.$$"
mv "$PREFIX/bin/lattice.new.$$" "$PREFIX/bin/lattice"

printf '\nInstalled lattice %s to %s/bin/lattice\n' "$ver" "$PREFIX"
case ":$PATH:" in
	*":$PREFIX/bin:"*) ;;
	*) printf '\n%s/bin is not on your PATH. Add it:\n    export PATH="%s/bin:$PATH"\n' "$PREFIX" "$PREFIX" ;;
esac
printf '\nNext: run `lattice` and press <CR> on "Tutor", or `lattice --scaffold-init` to start a config.\n'
printf 'Confirm the bundled plugins loaded with `:plugins` — three rows marked `bundled`.\n'
