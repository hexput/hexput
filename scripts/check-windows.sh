#!/usr/bin/env bash
# Compile-only cross-check of the whole workspace for Windows (x86_64-pc-windows-msvc): clippy
# with -D warnings over every crate and target, so the cfg(windows) code — the Named Pipe adapter
# (Story 5.3) and the Windows tests — is type-checked and linted on a non-Windows machine.
#
# Usage: scripts/check-windows.sh [extra cargo clippy arguments, e.g. --release]
#
# Needs the Rust target (`rustup target add x86_64-pc-windows-msvc`), `clang-cl` and `llvm-lib`
# (from LLVM). Nothing is linked or run: the C build scripts of `ring` and `alloca` only need to
# compile, which the stub headers below make possible without the Windows SDK. The tests
# themselves run on real Windows, in CI's `windows` job. Not a CI step.
set -euo pipefail

target=x86_64-pc-windows-msvc

# A tool on PATH by its plain name, or by a versioned name (`clang-cl-18`), newest first.
find_tool() {
    local name=$1
    if command -v "$name" >/dev/null 2>&1; then
        command -v "$name"
        return 0
    fi
    local candidate
    candidate=$(compgen -c "$name-" | grep -E "^$name-[0-9]+$" | sort -t- -k3 -n -r | head -n 1 || true)
    if [[ -n "$candidate" ]]; then
        command -v "$candidate"
        return 0
    fi
    return 1
}

missing=()
if ! rustup target list --installed 2>/dev/null | grep -qx "$target"; then
    missing+=("the Rust target: rustup target add $target")
fi
clang_cl=$(find_tool clang-cl) || missing+=("clang-cl: install LLVM/clang (e.g. apt install clang)")
llvm_lib=$(find_tool llvm-lib) || missing+=("llvm-lib: install LLVM (e.g. apt install llvm)")
if ((${#missing[@]})); then
    echo "check-windows.sh: missing prerequisites:" >&2
    for item in "${missing[@]}"; do
        echo "  - $item" >&2
    done
    exit 2
fi

stubs=$(mktemp -d)
trap 'rm -rf "$stubs"' EXIT

# Just enough of the C runtime's headers for `ring`'s and `alloca`'s C sources to compile.
cat >"$stubs/assert.h" <<'EOF'
#pragma once
#define assert(x) ((void)0)
EOF
cat >"$stubs/string.h" <<'EOF'
#pragma once
typedef unsigned long long size_t;
void *memcpy(void *dst, const void *src, size_t n);
void *memset(void *dst, int c, size_t n);
void *memmove(void *dst, const void *src, size_t n);
int memcmp(const void *a, const void *b, size_t n);
EOF
cat >"$stubs/stdlib.h" <<'EOF'
#pragma once
typedef unsigned long long size_t;
void abort(void);
unsigned long _byteswap_ulong(unsigned long value);
unsigned long long _byteswap_uint64(unsigned long long value);
unsigned short _byteswap_ushort(unsigned short value);
EOF
cat >"$stubs/malloc.h" <<'EOF'
#pragma once
typedef unsigned long long size_t;
void *_alloca(size_t size);
EOF

export CC_x86_64_pc_windows_msvc="$clang_cl"
export AR_x86_64_pc_windows_msvc="$llvm_lib"
export CFLAGS_x86_64_pc_windows_msvc="/imsvc $stubs"

cargo clippy --locked --target "$target" --workspace --all-targets "$@" -- -D warnings
