#!/usr/bin/env bash
# Run the ordinary release smoke with network syscalls unavailable. The smoke
# itself uses only local files, SQLite, subprocesses, and MCP over stdio.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /absolute/path/to/hsum" >&2
  exit 2
fi

binary="$1"
if [ ! -x "$binary" ]; then
  echo "hsum binary is not executable: $binary" >&2
  exit 2
fi

repository_root=$(cd "$(dirname "$0")/.." && pwd)
smoke="$repository_root/scripts/release-smoke.sh"

case "$(uname -s)" in
  Darwin)
    exec sandbox-exec \
      -p '(version 1) (allow default) (deny network*)' \
      bash "$smoke" "$binary"
    ;;
  Linux)
    if [ "$(id -u)" -eq 0 ]; then
      exec unshare --net bash "$smoke" "$binary"
    fi
    if sudo -n true 2>/dev/null; then
      exec sudo -n unshare --net \
        --setgid="$(id -g)" \
        --setuid="$(id -u)" \
        bash "$smoke" "$binary"
    fi
    echo "Linux no-network smoke requires root or passwordless sudo for unshare --net" >&2
    exit 2
    ;;
  *)
    echo "no-network smoke supports Linux and macOS" >&2
    exit 2
    ;;
esac
