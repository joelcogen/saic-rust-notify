#!/bin/sh
# Stage only manifests and Rust source from the service and sibling client.
set -eu
cd "$(dirname "$0")/.."
build_context=$(mktemp -d)
trap 'rm -rf "$build_context"' EXIT HUP INT TERM
mkdir "$build_context/saic-rust-notify" "$build_context/saic-rust-client"
cp Cargo.toml Cargo.lock "$build_context/saic-rust-notify/"
cp -R src "$build_context/saic-rust-notify/src"
cp ../saic-rust-client/Cargo.toml "$build_context/saic-rust-client/"
cp -R ../saic-rust-client/src "$build_context/saic-rust-client/src"
cp Dockerfile .dockerignore "$build_context/"
docker build -t "${1:-saic-rust-notify:local}" "$build_context"
