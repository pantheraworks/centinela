#!/usr/bin/env bash
set -euo pipefail

package="${1:?usage: firmware.sh <package> [cargo command] [args...]}"
shift

command="${1:-build}"
shift || true

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

case "$command" in
run | espflash*)
    for monitor in $(pgrep -f 'scripts/monitor\.sh' || true); do
        echo "stopping serial monitor $monitor so espflash can own the port"
        kill "$monitor" 2>/dev/null || true
        for _ in $(seq 1 50); do
            kill -0 "$monitor" 2>/dev/null || break
            sleep 0.1
        done
    done

    for port in /dev/cu.usbmodem*; do
        [ -e "$port" ] || continue
        for holder in $(lsof -t "$port" 2>/dev/null); do
            echo "stopping process $holder holding $port"
            kill "$holder" 2>/dev/null || true
        done
    done
    ;;
esac

export ESP_IDF_SYS_ROOT_CRATE="$package"
export CARGO_WORKSPACE_DIR="$repo_root"
export CARGO_TARGET_DIR="$repo_root/target/$package"

exec cargo "$command" -p "$package" "$@"
