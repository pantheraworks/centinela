#!/usr/bin/env bash
set -euo pipefail

cargo install espup
espup install

shell_name="$(basename "${SHELL:-/bin/zsh}")"
case "$shell_name" in
  zsh) rc_file="${ZDOTDIR:-$HOME}/.zshrc" ;;
  bash)
    if [[ -f "$HOME/.bashrc" ]]; then
      rc_file="$HOME/.bashrc"
    else
      rc_file="$HOME/.bash_profile"
    fi
    ;;
  *) rc_file="$HOME/.profile" ;;
esac

export_esp="$HOME/export-esp.sh"
if [[ ! -f "$export_esp" ]]; then
  echo "error: $export_esp was not created by espup install" >&2
  exit 1
fi

mkdir -p "$(dirname "$rc_file")"
touch "$rc_file"
if ! grep -qF "$export_esp" "$rc_file"; then
  printf '\n. "%s"\n' "$export_esp" >> "$rc_file"
fi

# shellcheck disable=SC1090
. "$export_esp"

cargo install ldproxy
cargo install espflash
cargo install cargo-generate
