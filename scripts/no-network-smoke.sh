#!/usr/bin/env bash
# Run the ordinary release smoke with network syscalls unavailable. The smoke
# itself uses only local files, SQLite, subprocesses, and MCP over stdio.
set -euo pipefail

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
  echo "usage: $0 /absolute/path/to/hsum [--binary-only|/absolute/path/to/signed-stable-bundle]" >&2
  exit 2
fi

binary="$1"
installer_input="${2:-}"
if [ ! -x "$binary" ]; then
  echo "hsum binary is not executable: $binary" >&2
  exit 2
fi

repository_root=$(cd "$(dirname "$0")/.." && pwd)
smoke="$repository_root/scripts/release-smoke.sh"
installer_smoke="$repository_root/scripts/installer-smoke.sh"
offline_smoke='set -e; bash "$1" "$3"; if [ "$4" = --binary-only ]; then exit 0; elif [ -n "$4" ]; then bash "$2" "$3" "$4"; else bash "$2" "$3"; fi'

case "$(uname -s)" in
  Darwin)
    exec sandbox-exec \
      -p '(version 1) (allow default) (deny network*)' \
      bash -c "$offline_smoke" _ "$smoke" "$installer_smoke" "$binary" "$installer_input"
    ;;
  Linux)
    if [ "$(id -u)" -eq 0 ]; then
      exec unshare --net \
        bash -c "$offline_smoke" _ "$smoke" "$installer_smoke" "$binary" "$installer_input"
    fi
    if sudo -n true 2>/dev/null; then
      exec sudo -n unshare --net \
        --setgid="$(id -g)" \
        --setuid="$(id -u)" \
        bash -c "$offline_smoke" _ "$smoke" "$installer_smoke" "$binary" "$installer_input"
    fi
    echo "Linux no-network smoke requires root or passwordless sudo for unshare --net" >&2
    exit 2
    ;;
  *)
    echo "no-network smoke supports Linux and macOS" >&2
    exit 2
    ;;
esac
