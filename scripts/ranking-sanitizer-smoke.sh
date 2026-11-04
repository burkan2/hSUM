#!/usr/bin/env bash
# Exercise the actual ranker modules in a small isolated sanitizer crate.
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 /absolute/path/to/new-evidence-directory" >&2
  exit 2
fi
evidence="$1"
test ! -e "$evidence" || { echo "evidence path already exists" >&2; exit 2; }
mkdir -p "$evidence"
evidence=$(cd "$evidence" && pwd)
repository_root=$(cd "$(dirname "$0")/.." && pwd)
trial_root=$(mktemp -d "${TMPDIR:-/tmp}/hsum-ranking-asan.XXXXXX")
trap 'rm -rf "$trial_root"' EXIT

case "$(uname -s):$(uname -m)" in
  Linux:x86_64) target=x86_64-unknown-linux-gnu; c_flags=-fsanitize=address ;;
  # Apple's C ASan runtime is incompatible with this Rust nightly runtime.
  # Rust loads/stores and allocator redzones remain instrumented on macOS.
  Darwin:arm64) target=aarch64-apple-darwin; c_flags='' ;;
  *) echo "unsupported sanitizer target" >&2; exit 2 ;;
esac

python3 - "$repository_root" "$trial_root" "$evidence" "$target" "$c_flags" <<'PY'
import hashlib, json, pathlib, shutil, sys, tomllib
repo, trial, evidence = map(pathlib.Path, sys.argv[1:4])
config = tomllib.loads((repo / 'Cargo.toml').read_text())
files = ['build.rs', 'src/search/bm25_math.c', 'src/search/positions.rs', 'src/search/ranking.rs']
for name in files:
    destination = trial / name
    destination.parent.mkdir(parents=True, exist_ok=True)
    shutil.copy2(repo / name, destination)
(trial / 'src/lib.rs').write_text('#![deny(unsafe_code)]\nmod search { mod positions; mod ranking; }\n')
shutil.copy2(repo / 'Cargo.lock', trial / 'Cargo.lock')
def value(item):
    if isinstance(item, dict):
        return '{ ' + ', '.join(f'{key} = {json.dumps(val)}' for key, val in item.items()) + ' }'
    return json.dumps(item)
manifest = '[package]\nname="hsum-ranking-asan"\nversion="0.0.0"\nedition="2024"\n'
for section, names in [('dependencies', ['rusqlite', 'libsqlite3-sys']),
                       ('dev-dependencies', ['proptest']), ('build-dependencies', ['cc', 'sha2'])]:
    manifest += f'[{section}]\n'
    for name in names:
        manifest += f'{name} = {value(config[section][name])}\n'
(trial / 'Cargo.toml').write_text(manifest)
(evidence / 'receipt.json').write_text(json.dumps({
    'toolchain': 'nightly-2026-08-20', 'target': sys.argv[4],
    'rust_sanitizer': 'address', 'c_sanitizer': 'address' if sys.argv[5] else None,
    'scope': 'actual counter/ranker source modules; excludes full application/model runtime; leak checks disabled',
    'files_sha256': {name: hashlib.sha256((trial / name).read_bytes()).hexdigest() for name in files},
}, indent=2) + '\n')
shutil.copy2(trial / 'Cargo.toml', evidence / 'harness-Cargo.toml')
PY

run_tests() {
  env -u LIBSQLITE3_SYS_USE_PKG_CONFIG \
    CARGO_TARGET_DIR="$trial_root/target" CARGO_INCREMENTAL=0 CARGO_PROFILE_TEST_DEBUG=1 \
    RUSTFLAGS='-Zsanitizer=address' CC=clang CFLAGS="$c_flags" ASAN_OPTIONS=detect_leaks=0 \
    cargo +nightly-2026-08-20 test --locked --manifest-path "$trial_root/Cargo.toml" --target "$target"
}

# Trim the copied lockfile to the isolated harness without changing versions.
cargo +nightly-2026-08-20 metadata --offline --format-version 1 --manifest-path "$trial_root/Cargo.toml" > /dev/null
cp "$trial_root/Cargo.lock" "$evidence/harness-Cargo.lock"
unset LIBSQLITE3_FLAGS
run_tests 2>&1 | tee "$evidence/enabled.log"
grep -Eq '^test search::ranking::tests::fast_and_forced_native_positions_match_builtin_bits_and_cover_fallbacks .* ok$' "$evidence/enabled.log"

# Even an empty custom-flags override must disable access to private bytes.
export LIBSQLITE3_FLAGS=''
run_tests 2>&1 | tee "$evidence/disabled.log"
if grep -Eq '^test search::ranking::tests::fast_and_forced_native_positions_match_builtin_bits_and_cover_fallbacks ' "$evidence/disabled.log"; then
  echo "custom SQLite build unexpectedly enabled the private-byte path" >&2
  exit 1
fi
printf 'enabled and disabled sanitizer checks passed\n' > "$evidence/result.txt"
