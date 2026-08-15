#!/usr/bin/env bash
set -uo pipefail

baud="${1:-115200}"

trap 'echo; echo "monitor stopped"; exit 0' INT

while true; do
    port=""
    until [ -n "$port" ]; do
        port="$(ls /dev/cu.usbmodem* 2>/dev/null | head -1)"
        [ -n "$port" ] || sleep 0.2
    done

    stty -f "$port" "$baud" raw -echo 2>/dev/null || true
    cat "$port" 2>/dev/null || true

    sleep 0.3
done
