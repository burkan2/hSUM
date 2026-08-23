#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$repo_root"

bash scripts/verify-cargo-publish-policy.sh

toolchain=${RUSTUP_TOOLCHAIN:-1.91.0}
version=$(cargo "+$toolchain" pkgid | sed -E 's/.*@([^@]+)$/\1/')
case "$version" in
  ''|*[!0-9A-Za-z.+-]*)
    echo "could not derive a safe package version" >&2
    exit 1
    ;;
esac

cargo "+$toolchain" package --locked --offline --allow-dirty
crate_path="$repo_root/target/package/hsum-$version.crate"
test -f "$crate_path" || { echo "package archive is missing: $crate_path" >&2; exit 1; }

listing=$(tar -tzf "$crate_path")
root="hsum-$version"
case "$listing" in
  *$'\n../'*|../*|*/../*)
    echo "package archive contains a parent traversal" >&2
    exit 1
    ;;
esac
while IFS= read -r entry; do
  case "$entry" in
    "$root"/*) ;;
    *)
      echo "package archive contains an entry outside $root: $entry" >&2
      exit 1
      ;;
  esac
done <<< "$listing"

required=(
  Cargo.lock
  Cargo.toml
  Cargo.toml.orig
  build.rs
  README.md
  LICENSE-APACHE
  LICENSE-MIT
  NOTICE
  src/lib.rs
  src/main.rs
  migrations/0001_alpha1.sql
  migrations/0004_vector_storage.sql
  assets/models/bge-small-en-v1.5-fp32.json
  examples/model-portability.rs
  benches/model_portability/inputs.json
  tests/search_contract.rs
)
for path in "${required[@]}"; do
  grep -Fqx "$root/$path" <<< "$listing" || {
    echo "package archive is missing required path: $path" >&2
    exit 1
  }
done

prohibited=(
  .claude/
  .github/
  docs/
  eval/
  outputs/
  work/
  target/
  benches/agent_ab/
  benches/sqlite_vec/
  benches/model_portability/results/
)
for path in "${prohibited[@]}"; do
  if grep -Fq "$root/$path" <<< "$listing"; then
    echo "package archive contains prohibited path: $path" >&2
    exit 1
  fi
done

if [ "${HSUM_PACKAGE_SKIP_INSTALL:-0}" != 1 ]; then
  trial_root=$(mktemp -d)
  trap 'rm -rf "$trial_root"' EXIT
  tar -xzf "$crate_path" -C "$trial_root"
  CARGO_TARGET_DIR="$repo_root/target" \
    cargo "+$toolchain" install \
      --locked \
      --path "$trial_root/$root" \
      --root "$trial_root/install"
  "$trial_root/install/bin/hsum" --version --verbose > "$trial_root/version.txt"
  test ! -e "$trial_root/install/bin/xtask" || {
    echo "source install exposed the internal xtask binary" >&2
    exit 1
  }
  grep -Fqx "hsum $version" "$trial_root/version.txt"
  grep -Fqx "Target: $(rustc "+$toolchain" -vV | sed -n 's/^host: //p')" \
    "$trial_root/version.txt"
fi

if command -v shasum >/dev/null 2>&1; then
  digest=$(shasum -a 256 "$crate_path" | awk '{print $1}')
else
  digest=$(sha256sum "$crate_path" | awk '{print $1}')
fi
echo "source package smoke passed: hsum-$version.crate sha256=$digest"
