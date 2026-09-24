#!/bin/sh
set -e
. "$HOME/.cargo/env"
cd "$(dirname "$0")/.."
cargo build --workspace
export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUNNER="sudo -n -E"
exec cargo test "$@"
