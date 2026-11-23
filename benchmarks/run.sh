#!/usr/bin/env bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"

# Ensure Rust and Go are in PATH if installed in standard user locations
export PATH="${HOME}/.cargo/bin:${HOME}/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:/usr/local/go/bin:${PATH}"

exec python3 "${SCRIPT_DIR}/run.py" "$@"
