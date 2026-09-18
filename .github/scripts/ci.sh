#!/usr/bin/env bash

set -euo pipefail

readonly REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

if ! cargo clippy --version >/dev/null 2>&1; then
  echo "Full emulator CI: Clippy is required for static analysis." >&2
  exit 1
fi

cargo clippy --locked --all-targets --manifest-path "$REPO_DIR/Cargo.toml" -- \
  -D warnings -A clippy::style -A clippy::complexity
cargo test --manifest-path "$REPO_DIR/Cargo.toml"
cargo test --release --manifest-path "$REPO_DIR/Cargo.toml"
