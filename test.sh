#!/usr/bin/env bash
set -euo pipefail

host="$(rustc +stable -vV | awk '/^host:/{print $2}')"
exec cargo +stable test --lib --target "$host" --config 'unstable.build-std=[]' "$@"
