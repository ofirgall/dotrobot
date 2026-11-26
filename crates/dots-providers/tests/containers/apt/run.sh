#!/bin/sh
# Runs inside an ubuntu:<ver> container with the workspace at /work (read-only) and a
# persistent /cache volume (rustup, cargo registry, target dir). Never run this on a host.
set -eu
[ -f /.dockerenv ] || { echo "refusing: not in a container" >&2; exit 2; }
export DEBIAN_FRONTEND=noninteractive
export RUSTUP_HOME=/cache/rustup CARGO_HOME=/cache/cargo CARGO_TARGET_DIR=/cache/target
export PATH="/cache/cargo/bin:$PATH"

apt-get update -qq
apt-get install -y -qq curl ca-certificates gcc >/dev/null
if ! command -v cargo >/dev/null; then
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null 2>&1
fi
. /etc/os-release
echo "== $PRETTY_NAME, $(rustc --version)"

# Build first (needs network for crates), then leave the package lists empty so the tests
# start from a stale cache like a fresh machine.
cd /work
cargo test -p dots-providers --test apt --no-run --locked 2>&1 | tail -3
rm -rf /var/lib/apt/lists/*
export DOTS_APT_CONTAINER=1
# 1) lifecycle first: empty lists -> exercises "refresh stale cache and retry".
cargo test -p dots-providers --test apt --locked -- --ignored --exact real_lifecycle --nocapture
# 2) conformance on a warm cache (it demands exec runs exactly one command).
cargo test -p dots-providers --test apt --locked -- --ignored --exact real_conformance --nocapture
