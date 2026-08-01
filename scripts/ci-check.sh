#!/usr/bin/env bash
# Thin shim for `cex ci-check --rust`, kept so `bash scripts/ci-check.sh` and any
# editor run-task pointing at this path keep working.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")"
exec cex ci-check --rust
