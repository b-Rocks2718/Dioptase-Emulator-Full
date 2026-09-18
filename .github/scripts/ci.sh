#!/usr/bin/env bash

set -euo pipefail

readonly REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

cargo test --manifest-path "$REPO_DIR/Cargo.toml"
cargo test --release --manifest-path "$REPO_DIR/Cargo.toml"
