#!/usr/bin/env bash
# Exercise the release-rendered installer without network or real Codex state.
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
version=$(awk -F '"' '$1 ~ /^version = / { print $2; exit }' "$repository_root/Cargo.toml")
trial_root=$(mktemp -d "${TMPDIR:-/tmp}/hsum-installer-smoke.XXXXXX")
cleanup() {
  rm -rf "$trial_root"
}
trap cleanup EXIT

rendered_installer="$trial_root/install-hsum.sh"
sed "s/@HSUM_VERSION@/$version/g" "$repository_root/scripts/install.sh" > "$rendered_installer"
chmod 700 "$rendered_installer"
bash -n "$rendered_installer"

case "$(uname -s):$(uname -m)" in
  Darwin:arm64)
    target="aarch64-apple-darwin"
    archive_kind="zip"
    ;;
  Linux:x86_64)
    target="x86_64-unknown-linux-gnu"
    archive_kind="tar.gz"
    ;;
  *)
    echo "installer smoke supports Linux x86_64 and macOS arm64" >&2
    exit 2
    ;;
esac

tag="v$version"
asset="hsum-$tag-$target.$archive_kind"
fixtures="$trial_root/fixtures"
archive_root="$trial_root/archive-root"
mkdir -p "$fixtures" "$archive_root"
cp "$binary" "$archive_root/hsum"
chmod 700 "$archive_root/hsum"
if [ "$archive_kind" = "zip" ]; then
  (
    cd "$archive_root"
    zip -X -q "$fixtures/$asset" hsum
  )
else
  tar -C "$archive_root" -czf "$fixtures/$asset" hsum
fi
(
  cd "$fixtures"
  shasum -a 256 "$asset" > "$asset.sha256"
)

fake_bin="$trial_root/fake-bin"
mkdir "$fake_bin"
cat > "$fake_bin/curl" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
output=""
url=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --output)
      output="$2"
      shift 2
      ;;
    http*)
      url="$1"
      shift
      ;;
    --proto|--tlsv1.2)
      if [ "$1" = "--proto" ]; then shift 2; else shift; fi
      ;;
    *)
      shift
      ;;
  esac
done
[ -n "$output" ] && [ -n "$url" ]
cp "$HSUM_INSTALL_FIXTURES/${url##*/}" "$output"
EOF

cat > "$fake_bin/codex" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
registration="$HSUM_FAKE_CODEX_REGISTRATION"
if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "get" ]; then
  if [ ! -f "$registration" ]; then
    echo "Error: No MCP server named 'hsum' found." >&2
    exit 1
  fi
  command=$(cat "$registration")
  printf '{"name":"hsum","enabled":true,"transport":{"type":"stdio","command":"%s","args":["mcp"],"env":null,"env_vars":[],"cwd":null},"enabled_tools":null,"disabled_tools":null}\n' "$command"
  exit 0
fi
if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "add" ]; then
  printf '%s' "${5:-}" > "$registration"
  echo "Added global MCP server 'hsum'."
  exit 0
fi
if [ "${1:-}" = "mcp" ] && [ "${2:-}" = "remove" ]; then
  rm -f "$registration"
  echo "Removed global MCP server 'hsum'."
  exit 0
fi
echo "unsupported fake Codex invocation" >&2
exit 2
EOF
chmod 700 "$fake_bin/curl" "$fake_bin/codex"

repository="$trial_root/repository"
mkdir "$repository"
git init --quiet "$repository"
printf 'InstallerEvidence is locally indexed.\n' > "$repository/README.md"

export HSUM_INSTALL_FIXTURES="$fixtures"
export HSUM_FAKE_CODEX_REGISTRATION="$trial_root/codex-registration"
export HSUM_HOME="$trial_root/hsum-home"
export CODEX_HOME="$trial_root/codex-home"
export XDG_BIN_HOME="$trial_root/user-bin"
export PATH="$fake_bin:$PATH"

installer_arguments=(--confirm --activate "$repository")
if [ "$(uname -s)" = "Darwin" ]; then
  installer_arguments+=(--allow-unnotarized-alpha)
fi
"$rendered_installer" "${installer_arguments[@]}" > "$trial_root/install.txt"

installed="$(cd "$XDG_BIN_HOME" && pwd -P)/hsum"
test -x "$installed"
test "$(cat "$HSUM_FAKE_CODEX_REGISTRATION")" = "$installed"
grep -Fq 'Citation round trip: verified' "$trial_root/install.txt"
"$installed" integration status codex --json > "$trial_root/status.json"
python3 - "$trial_root/status.json" "$installed" <<'PY'
import json
import pathlib
import sys

status = json.loads(pathlib.Path(sys.argv[1]).read_text())
assert status["registration"] == "current"
assert status["registered_executable"] == sys.argv[2]
assert status["registered_arguments"] == ["mcp"]
assert status["agent_policy"] == "current"
assert status["next_actions"] == []
PY

"$rendered_installer" "${installer_arguments[@]}" > "$trial_root/reinstall.txt"
grep -Fq 'Registration changed: no' "$trial_root/reinstall.txt"
test ! -e "$repository/.hsum.toml"

bad_fixtures="$trial_root/bad-fixtures"
mkdir "$bad_fixtures"
cp "$fixtures/$asset" "$bad_fixtures/$asset"
printf '%064d  %s\n' 0 "$asset" > "$bad_fixtures/$asset.sha256"
export HSUM_INSTALL_FIXTURES="$bad_fixtures"
export XDG_BIN_HOME="$trial_root/refused-bin"
set +e
"$rendered_installer" "${installer_arguments[@]}" \
  > "$trial_root/refused.txt" 2> "$trial_root/refused-error.txt"
refused_exit=$?
set -e
test "$refused_exit" -ne 0
test ! -e "$XDG_BIN_HOME/hsum"

echo "installer smoke passed"
