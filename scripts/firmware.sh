#!/usr/bin/env bash
set -euo pipefail

package="${1:?usage: firmware.sh <package> [cargo command] [args...]}"
shift

command="${1:-build}"
shift || true

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

export ESP_IDF_SYS_ROOT_CRATE="$package"
export CARGO_WORKSPACE_DIR="$repo_root"
export CARGO_TARGET_DIR="$repo_root/target/$package"

exec cargo "$command" -p "$package" "$@"
