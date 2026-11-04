#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 || ! -f "$1" ]]; then
  echo "usage: drop-cache-macos.sh /absolute/path/to/index.sqlite" >&2
  exit 2
fi

sudo -n /usr/sbin/purge
