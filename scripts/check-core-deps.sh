#!/usr/bin/env bash
set -euo pipefail

forbidden='^(esp-|embuild|embassy)'

tree="$(cargo +stable tree -p centinela-core --edges normal --target all --prefix none --config 'unstable.build-std=[]')"

if matches="$(grep -E "$forbidden" <<<"$tree")"; then
    echo "centinela-core must not depend on ESP crates:" >&2
    echo "$matches" >&2
    exit 1
fi
