#!/usr/bin/env bash
set -e
# Rust toolchain (stable), musl target, mold, flamegraph and podman are
# pre-installed in the Dockerfile for cache + Zed (postCreateCommand is skipped
# in some editors). What is left is warming the cargo cache, so the first real
# build of the service does not have to fetch the world.
cd "$(dirname "$0")/.."
cargo fetch --locked
