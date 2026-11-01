#!/usr/bin/env bash
# Release-rendered, checksum-verifying hSUM installer.
set -euo pipefail

version="@HSUM_VERSION@"
minisign_public_key="@HSUM_MINISIGN_PUBLIC_KEY@"
template_version_marker="@HSUM""_VERSION@"
repository="burkan2/hSUM"
activate_path=""
confirmed=false
allow_unnotarized_alpha=false

usage() {
  printf '%s\n' \
    "usage: install-hsum.sh --confirm [--activate PATH] [--allow-unnotarized-alpha]" \
    "" \
    "Installs a pinned hSUM release into XDG_BIN_HOME or ~/.local/bin, then" \
    "registers Codex and activates the selected repository (default: current Git root)."
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --activate)
      [ "$#" -ge 2 ] || { usage >&2; exit 2; }
      activate_path="$2"
      shift 2
      ;;
    --confirm)
      confirmed=true
      shift
      ;;
    --allow-unnotarized-alpha)
      allow_unnotarized_alpha=true
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      printf 'unknown installer argument: %s\n' "$1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

if [ "$version" = "$template_version_marker" ]; then
  printf '%s\n' \
    "This source template is not an installer." \
    "Download install-hsum.sh from a specific hSUM GitHub Release." >&2
  exit 2
fi

stable_release=false
if [[ "$version" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)$ ]]; then
  stable_release=true
  if [ "$allow_unnotarized_alpha" = true ]; then
    printf 'stable releases cannot bypass macOS notarization\n' >&2
    exit 2
  fi
  if [[ ! "$minisign_public_key" =~ ^RW[A-Za-z0-9+/]{54}$ ]]; then
    printf 'stable installer has no valid pinned Minisign public key\n' >&2
    exit 2
  fi
  command -v minisign >/dev/null 2>&1 || {
    printf 'stable installation requires Minisign for signature verification\n' >&2
    exit 2
  }
elif [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+-alpha\.[0-9]+$ ]]; then
  printf 'unsupported release version: %s\n' "$version" >&2
  exit 2
fi

if [ "$confirmed" != true ]; then
  printf '%s\n' \
    "Installation and repository activation require confirmation." \
    "Review the repository root and repeat with --confirm." >&2
  exit 2
fi

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
    printf 'unsupported hSUM installer platform: %s %s\n' "$(uname -s)" "$(uname -m)" >&2
    exit 2
    ;;
esac

if [ -n "${XDG_BIN_HOME:-}" ]; then
  bin_dir="$XDG_BIN_HOME"
else
  [ -n "${HOME:-}" ] || { printf 'HOME is unavailable\n' >&2; exit 2; }
  bin_dir="$HOME/.local/bin"
fi
case "$bin_dir" in
  /*) ;;
  *)
    printf 'the hSUM install directory must be absolute: %s\n' "$bin_dir" >&2
    exit 2
    ;;
esac

if [ -z "$activate_path" ]; then
  activate_path=$(pwd -P)
fi

tag="v$version"
asset="hsum-$tag-$target.$archive_kind"
base_url="https://github.com/$repository/releases/download/$tag"
install_tmp=$(mktemp -d "${TMPDIR:-/tmp}/hsum-install.XXXXXX")
candidate=""
cleanup() {
  if [ -n "$candidate" ] && [ -e "$candidate" ]; then
    rm -f "$candidate"
  fi
  rm -rf "$install_tmp"
}
trap cleanup EXIT HUP INT TERM

archive="$install_tmp/$asset"
checksum="$install_tmp/$asset.sha256"
if [ "$stable_release" = true ]; then
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$install_tmp/SHA256SUMS" "$base_url/SHA256SUMS"
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$install_tmp/SHA256SUMS.minisig" "$base_url/SHA256SUMS.minisig"
  verified_comment=$(minisign -V -H -Q -m "$install_tmp/SHA256SUMS" -P "$minisign_public_key" \
    -x "$install_tmp/SHA256SUMS.minisig")
  test "$verified_comment" = "hSUM $tag: SHA256SUMS"
  # Select exactly one fixed archive name from the authenticated manifest;
  # never pass arbitrary manifest paths to the checksum tool.
  awk -v expected="$asset" '
    $2 == expected {
      if (NF != 2 || length($1) != 64 || $1 ~ /[^0-9a-f]/) exit 1
      found++; print $1 "  " expected
    }
    END { if (found != 1) exit 1 }
  ' "$install_tmp/SHA256SUMS" > "$checksum"
fi
curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
  --output "$archive" "$base_url/$asset"
if [ "$stable_release" = true ]; then
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$archive.minisig" "$base_url/$asset.minisig"
  verified_comment=$(minisign -V -H -Q -m "$archive" -P "$minisign_public_key" -x "$archive.minisig")
  test "$verified_comment" = "hSUM $tag: $asset"
else
  curl --proto '=https' --tlsv1.2 --fail --location --silent --show-error \
    --output "$checksum" "$base_url/$asset.sha256"
fi

(
  cd "$install_tmp"
  shasum -a 256 -c "$asset.sha256"
)

extract_dir="$install_tmp/extracted"
mkdir "$extract_dir"
case "$archive_kind" in
  zip)
    entries=$(unzip -Z1 "$archive")
    [ "$entries" = "hsum" ] || {
      printf 'release archive must contain exactly one hsum binary\n' >&2
      exit 1
    }
    unzip -q "$archive" -d "$extract_dir"
    ;;
  tar.gz)
    entries=$(tar -tzf "$archive")
    [ "$entries" = "hsum" ] || {
      printf 'release archive must contain exactly one hsum binary\n' >&2
      exit 1
    }
    tar -xzf "$archive" -C "$extract_dir"
    ;;
esac

extracted="$extract_dir/hsum"
[ -f "$extracted" ] && [ ! -L "$extracted" ] || {
  printf 'verified archive did not produce one regular hsum binary\n' >&2
  exit 1
}
chmod 700 "$extracted"

if [ "$(uname -s)" = "Darwin" ] && ! spctl --assess --type execute "$extracted" 2>/dev/null; then
  if [ "$stable_release" = true ]; then
    printf 'macOS rejected the stable Developer ID/notarization assessment\n' >&2
    exit 2
  fi
  if [ "$allow_unnotarized_alpha" != true ]; then
    printf '%s\n' \
      "The verified macOS alpha is not Developer ID-notarized." \
      "After reviewing this limitation, repeat with --allow-unnotarized-alpha." >&2
    exit 2
  fi
  xattr -d com.apple.quarantine "$extracted" 2>/dev/null || true
fi

"$extracted" --version --verbose > "$install_tmp/version.txt"
grep -Fqx "hsum $version" <(head -n 1 "$install_tmp/version.txt")

mkdir -p "$bin_dir"
destination="$bin_dir/hsum"
if [ -L "$destination" ] || { [ -e "$destination" ] && [ ! -f "$destination" ]; }; then
  printf 'refusing to replace a non-regular hSUM destination: %s\n' "$destination" >&2
  exit 1
fi

candidate=$(mktemp "$bin_dir/.hsum.install.XXXXXX")
if [ "$(uname -s)" = "Darwin" ]; then
  ditto "$extracted" "$candidate"
else
  cp "$extracted" "$candidate"
fi
chmod 700 "$candidate"
"$candidate" --version --verbose > "$install_tmp/candidate-version.txt"
grep -Fqx "hsum $version" <(head -n 1 "$install_tmp/candidate-version.txt")

backup=""
if [ -f "$destination" ]; then
  backup="$install_tmp/previous-hsum"
  if [ "$(uname -s)" = "Darwin" ]; then
    ditto "$destination" "$backup"
  else
    cp "$destination" "$backup"
  fi
  chmod 700 "$backup"
fi

mv -f "$candidate" "$destination"
candidate=""
if ! "$destination" --version --verbose > "$install_tmp/installed-version.txt"; then
  if [ -n "$backup" ]; then
    restore=$(mktemp "$bin_dir/.hsum.restore.XXXXXX")
    cp "$backup" "$restore"
    chmod 700 "$restore"
    mv -f "$restore" "$destination"
  fi
  printf 'the installed hSUM probe failed; the previous binary was restored\n' >&2
  exit 1
fi
grep -Fqx "hsum $version" <(head -n 1 "$install_tmp/installed-version.txt")

printf 'Installed hSUM %s at %s\n' "$version" "$destination"
"$destination" integration install codex \
  --activate "$activate_path" \
  --confirm
