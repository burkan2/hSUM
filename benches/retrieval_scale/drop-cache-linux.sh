#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 || ! -f "$1" ]]; then
  echo "usage: drop-cache-linux.sh /absolute/path/to/index.sqlite" >&2
  exit 2
fi

sync
sudo -n sh -c 'echo 3 > /proc/sys/vm/drop_caches'
