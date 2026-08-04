#!/usr/bin/env bash
# The gate every commit has to pass: fmt, clippy, tests, doctests, docs, audit.
#
# **This runs exactly what CI runs, and nothing else.** A gate that checks something
# adjacent to what CI checks is worse than no gate: it returns green, the commit is
# pushed on the strength of it, and the failure arrives anyway with the local check
# still insisting it passed.
#
# That is not hypothetical. This script used to delegate to `cex ci-check --rust` when
# that tool was installed, and its clippy runs without `--all-targets` - so no
# `#[cfg(test)]` code was ever linted. Six red runs were pushed against a green local
# gate before the difference was measured. `TYCHO_CI_TABLE=1` still opts into that
# tool's rendering, but it is not the gate.
#
# This gate runs the host's own target only, so it cannot see the other platform.
# `TYCHO_CROSS=1` adds a lint-only pass over the other one, which catches everything
# except behaviour: a `#[cfg]`-gated arm that does not compile, and a lint that fires
# there and not here. It cannot catch a test that fails only on that host - the
# `#[cfg(windows)]` symlink and DACL tests each cost a CI round for exactly that.

set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [ "${TYCHO_CI_TABLE:-}" = "1" ] && command -v cex >/dev/null 2>&1; then
    exec cex ci-check --rust --root "$root"
fi

cd "$root"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT
fail=0

run() {
    local name="$1"
    shift
    if "$@" >"$log" 2>&1; then
        printf '  ok    %s\n' "$name"
    else
        printf '  FAIL  %s\n' "$name"
        sed 's/^/        /' "$log"
        fail=1
    fi
}

printf 'ci-check  the same six checks CI runs\n'
run fmt     cargo fmt --all --check
run clippy  cargo clippy --workspace --all-targets --all-features -- -D warnings
# --run-ignored=all because `#[ignore]` is not an escape hatch here. `cargo test`
# is the fallback when nextest is absent: it runs the same tests, but in threads
# rather than a process each, so a test that mutates process-global state can see
# another's. `cargo install cargo-nextest` is the supported path.
if command -v cargo-nextest >/dev/null 2>&1; then
    run test cargo nextest run --workspace --all-features --run-ignored=all
else
    printf '  note  nextest not installed; using cargo test (cargo install cargo-nextest)\n'
    run test cargo test --workspace --all-features -- --include-ignored
fi
run doctest cargo test --workspace --doc
run doc     cargo doc --workspace --no-deps
if command -v cargo-audit >/dev/null 2>&1; then
    run audit cargo audit
else
    printf '  skip  audit (cargo install cargo-audit)\n'
fi

if [ "${TYCHO_CROSS:-}" = "1" ]; then
    case "$(uname -s 2>/dev/null || echo Windows)" in
        Darwin|Linux) other=x86_64-pc-windows-msvc ;;
        *) other=aarch64-apple-darwin ;;
    esac
    if rustup target list --installed 2>/dev/null | grep -qx "$other"; then
        run "cross ($other)" cargo clippy --target "$other" --workspace --all-targets \
            --all-features -- -D warnings
    else
        printf '  skip  cross (rustup target add %s)\n' "$other"
    fi
fi

if [ "$fail" -ne 0 ]; then
    printf '\nci-check failed\n'
    exit 1
fi
printf '\nall checks passed\n'
