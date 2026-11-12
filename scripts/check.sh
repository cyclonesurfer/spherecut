#!/bin/sh
# Same as `cargo xtask check`, which CI can run too (.github/workflows/check.yml, D78; manual until Pat turns it on).
set -eu
cd "$(dirname "$0")/.."
exec cargo xtask check "$@"
