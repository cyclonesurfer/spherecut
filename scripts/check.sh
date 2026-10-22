#!/bin/sh
# Same as `cargo xtask check`; kept for muscle memory and CI.
set -eu
cd "$(dirname "$0")/.."
exec cargo xtask check "$@"
