#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 1 ]; then
  echo "usage: $0 OUTPUT.json" >&2
  exit 2
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
output=$1
case "$output" in
  /*) ;;
  *) output="$PWD/$output" ;;
esac

output_dir=$(dirname -- "$output")
mkdir -p "$output_dir"

metadata_file=$(mktemp "${TMPDIR:-/tmp}/hsum-cargo-metadata.XXXXXX")
trap 'rm -f "$metadata_file"' EXIT

toolchain=${RUSTUP_TOOLCHAIN:-1.91.0}
(
  cd "$repository_root"
  cargo "+$toolchain" metadata --locked --offline --all-features --format-version 1 \
    > "$metadata_file"
)

python3 - \
  "$metadata_file" \
  "$repository_root/Cargo.lock" \
  "$repository_root/Cargo.toml" \
  "$output" <<'PY'
import hashlib
import json
import os
from pathlib import Path
import sys
import tempfile

metadata_path = Path(sys.argv[1])
lock_path = Path(sys.argv[2])
manifest_path = Path(sys.argv[3])
output_path = Path(sys.argv[4])

metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
root_id = metadata.get("resolve", {}).get("root")
root_package = next(
    (package for package in metadata["packages"] if package["id"] == root_id),
    None,
)
if root_package is None:
    raise SystemExit("cargo metadata did not identify the workspace root package")

packages = []
missing = []
for package in metadata["packages"]:
    declared_license = package.get("license")
    declared_license_file = package.get("license_file")
    if not declared_license and not declared_license_file:
        missing.append(f"{package['name']}@{package['version']}")
        continue

    license_file = None
    if declared_license_file:
        manifest_parent = Path(package["manifest_path"]).parent.resolve()
        license_path = Path(declared_license_file)
        if not license_path.is_absolute():
            license_path = manifest_parent / license_path
        try:
            license_file = (
                license_path.resolve().relative_to(manifest_parent).as_posix()
            )
        except ValueError as error:
            raise SystemExit(
                f"{package['name']}@{package['version']} has a license file "
                "outside its package directory"
            ) from error

    packages.append(
        {
            "name": package["name"],
            "version": package["version"],
            "source": package.get("source") or "workspace",
            "license": declared_license,
            "license_file": license_file,
        }
    )

if missing:
    raise SystemExit(
        "packages without a declared license or license file: " + ", ".join(sorted(missing))
    )

packages.sort(
    key=lambda package: (
        package["name"],
        package["version"],
        package["source"],
        package["license"] or "",
        package["license_file"] or "",
    )
)

payload = {
    "schema_version": "hsum.cargo-license-inventory.v1",
    "package": {
        "name": root_package["name"],
        "version": root_package["version"],
    },
    "cargo_toml_sha256": hashlib.sha256(manifest_path.read_bytes()).hexdigest(),
    "cargo_lock_sha256": hashlib.sha256(lock_path.read_bytes()).hexdigest(),
    "scope": (
        "All workspace, runtime, build, development, and target-specific packages "
        "returned by cargo metadata --locked --offline --all-features."
    ),
    "notice": (
        "This records package-authored Cargo license declarations. It is release "
        "evidence, not a legal conclusion."
    ),
    "package_count": len(packages),
    "packages": packages,
}

output_path.parent.mkdir(parents=True, exist_ok=True)
temporary_name = None
try:
    with tempfile.NamedTemporaryFile(
        mode="w",
        encoding="utf-8",
        dir=output_path.parent,
        prefix=f".{output_path.name}.",
        delete=False,
    ) as temporary:
        temporary_name = temporary.name
        json.dump(payload, temporary, ensure_ascii=False, indent=2)
        temporary.write("\n")
    os.replace(temporary_name, output_path)
    os.chmod(output_path, 0o644)
except BaseException:
    if temporary_name is not None:
        Path(temporary_name).unlink(missing_ok=True)
    raise
PY

echo "wrote $output"
