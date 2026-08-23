#!/usr/bin/env bash
#
# Build a tree-sitter grammar to a WebAssembly side module — WITHOUT
# emscripten, docker, or the tree-sitter CLI.
#
#   scripts/build-wasm-grammar.sh <name> <grammar-src-dir> [out-dir]
#
#     name             the grammar's tree-sitter name, i.e. the suffix of its
#                      `tree_sitter_<name>` entry point ("markdown", "org").
#     grammar-src-dir  the directory holding `parser.c` (and `scanner.c`, if
#                      the grammar has an external scanner) plus its
#                      `tree_sitter/` headers.
#     out-dir          defaults to `target/wasm-grammars`.
#
# ## Why this exists rather than `tree-sitter build --wasm`
#
# The documented route needs emscripten (~1 GB) or a docker daemon, which
# would make either a prerequisite for anyone building or benchmarking this
# repo. It turns out neither is needed, because of what tree-sitter's wasm
# store actually requires of a grammar module (`tree-sitter/src/wasm_store.c`):
#
#   * The module is a plain **wasm side module** — a `dylink.0` custom
#     section, which `wasm-ld -shared` emits natively. Nothing about the
#     format is emscripten-specific.
#   * The store itself supplies `memory`, `__stack_pointer`, `__memory_base`,
#     `__table_base` and `__indirect_function_table` as builtin imports, and
#     ships a **prebuilt wasm libc** (`src/wasm/wasm-stdlib.h`) exporting the
#     24 symbols in `src/wasm/stdlib-symbols.txt` (malloc/free/memcpy/isw*/…).
#
# So the grammar needs no libc of its own — only *declarations* for the
# handful of functions it calls. That is what `sysroot/` below is: ~60 lines
# of headers generated inline, covering exactly tree-sitter's published
# stdlib surface. Everything else (`stdint.h`, `stdbool.h`, `stddef.h`) comes
# from clang's freestanding headers.
#
# The linker is `rust-lld`, which every rustup toolchain already ships and
# which dispatches to its wasm driver when invoked as `wasm-ld` (LLD selects
# its flavour from argv[0]). So the full toolchain is: **clang + rustup**.
#
# ## The two flags that are not obvious
#
#   --experimental-pic -shared   emit a PIC side module with `dylink.0`.
#   --Bsymbolic                  bind defined symbols locally. WITHOUT this,
#                                the external-scanner entry points stay
#                                preemptible, so LLD emits `GOT.func.<name>`
#                                imports for them — and tree-sitter's store
#                                resolves only its own builtin and stdlib
#                                names, so instantiation fails with
#                                "invalid import
#                                'tree_sitter_<name>_external_scanner_create'".
#                                The symbols are defined *in the module*; the
#                                error is purely about symbol binding.
#
set -euo pipefail

if [[ $# -lt 2 ]]; then
	sed -n '2,12p' "$0" >&2
	exit 2
fi

NAME="$1"
SRC="$2"
OUT="${3:-target/wasm-grammars}"

CLANG="${CLANG:-$(command -v clang)}"
if [[ -z "$CLANG" ]]; then
	echo "build-wasm-grammar: no clang on PATH" >&2
	exit 1
fi

# rust-lld ships with every rustup toolchain; LLD picks its driver from
# argv[0], so a `wasm-ld` symlink is the whole of the wasm linker setup.
RUST_SYSROOT="$(rustc --print sysroot)"
HOST="$(rustc -vV | sed -n 's/^host: //p')"
RUST_LLD="$RUST_SYSROOT/lib/rustlib/$HOST/bin/rust-lld"
if [[ ! -x "$RUST_LLD" ]]; then
	echo "build-wasm-grammar: rust-lld not found at $RUST_LLD" >&2
	exit 1
fi

if [[ ! -f "$SRC/parser.c" ]]; then
	echo "build-wasm-grammar: no parser.c in $SRC" >&2
	exit 1
fi

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p "$WORK/bin" "$WORK/sysroot/include" "$WORK/obj" "$OUT"
ln -sf "$RUST_LLD" "$WORK/bin/wasm-ld"

# ── the shim sysroot ────────────────────────────────────────────────
# Declarations only, matching tree-sitter's stdlib-symbols.txt exactly.
# No definitions: every one of these resolves to the store's wasm libc
# at instantiation time.
I="$WORK/sysroot/include"
cat >"$I/stdlib.h" <<'EOF'
#pragma once
#include <stddef.h>
void *malloc(size_t);
void *calloc(size_t, size_t);
void *realloc(void *, size_t);
void free(void *);
_Noreturn void abort(void);
EOF
cat >"$I/string.h" <<'EOF'
#pragma once
#include <stddef.h>
void *memchr(const void *, int, size_t);
int memcmp(const void *, const void *, size_t);
void *memcpy(void *, const void *, size_t);
void *memmove(void *, const void *, size_t);
void *memset(void *, int, size_t);
int strcmp(const char *, const char *);
size_t strlen(const char *);
char *strncat(char *, const char *, size_t);
int strncmp(const char *, const char *, size_t);
char *strncpy(char *, const char *, size_t);
EOF
cat >"$I/wchar.h" <<'EOF'
#pragma once
#include <stddef.h>
typedef int wint_t;
EOF
cat >"$I/wctype.h" <<'EOF'
#pragma once
#include <wchar.h>
typedef int wctype_t;
int iswalnum(wint_t); int iswalpha(wint_t); int iswblank(wint_t);
int iswdigit(wint_t); int iswlower(wint_t); int iswspace(wint_t);
int iswupper(wint_t); int iswxdigit(wint_t);
wint_t towlower(wint_t); wint_t towupper(wint_t);
EOF
cat >"$I/ctype.h" <<'EOF'
#pragma once
int isalnum(int); int isalpha(int); int isdigit(int);
int islower(int); int isspace(int); int isupper(int); int isxdigit(int);
int tolower(int); int toupper(int);
EOF
cat >"$I/stdio.h" <<'EOF'
#pragma once
#include <stddef.h>
int printf(const char *, ...);
int fprintf(void *, const char *, ...);
EOF
cat >"$I/assert.h" <<'EOF'
#pragma once
_Noreturn void __assert_fail(const char *, const char *, unsigned, const char *);
#ifdef NDEBUG
#define assert(x) ((void)0)
#else
#define assert(x) ((x) ? (void)0 : __assert_fail(#x, __FILE__, __LINE__, __func__))
#endif
EOF

OBJS=()
for f in parser scanner; do
	[[ -f "$SRC/$f.c" ]] || continue
	"$CLANG" --target=wasm32-unknown-unknown \
		-fPIC -fvisibility=default -nostdlibinc -nostdlib \
		-O2 -DNDEBUG \
		-isystem "$WORK/sysroot/include" -I "$SRC" \
		-c "$SRC/$f.c" -o "$WORK/obj/$f.o"
	OBJS+=("$WORK/obj/$f.o")
done

DEST="$OUT/tree-sitter-$NAME.wasm"
"$WORK/bin/wasm-ld" \
	--experimental-pic -shared --Bsymbolic \
	--no-entry --export-dynamic --allow-undefined \
	-o "$DEST" "${OBJS[@]}"

echo "build-wasm-grammar: wrote $DEST ($(wc -c <"$DEST" | tr -d ' ') bytes)"
