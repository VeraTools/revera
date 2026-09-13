#!/usr/bin/env bash
set -euo pipefail

cargo_version=$(
  sed -n '/^\[package\]/,/^\[/ s/^version = "\([^"]*\)"/\1/p' Cargo.toml |
    head -n 1
)
action_version=$(
  sed -n '/^  revera-version:/,/^  [^ ]/ s/^    default: "\([^"]*\)"/\1/p' action.yml |
    head -n 1
)

if [[ -z "$cargo_version" || -z "$action_version" ]]; then
  echo "could not determine Cargo or Action Revera version" >&2
  exit 1
fi

if [[ "$cargo_version" != "$action_version" ]]; then
  echo "Revera version mismatch: Cargo.toml=$cargo_version action.yml=$action_version" >&2
  exit 1
fi

echo "Revera versions match: $cargo_version"
