#!/usr/bin/env python3
"""Verify cargo-binstall metadata against hSUM's supported release assets."""

from __future__ import annotations

import argparse
import json
import pathlib
import tomllib
from typing import Any


TARGETS = {
    "aarch64-apple-darwin": ("zip", ".zip"),
    "x86_64-unknown-linux-gnu": ("tgz", ".tar.gz"),
}
URL_PREFIX = (
    "{ repo }/releases/download/v{ version }/"
    "{ name }-v{ version }-{ target }"
)


class MetadataError(RuntimeError):
    """The cargo-binstall metadata no longer matches the release contract."""


def verify(cargo_toml: bytes) -> dict[str, Any]:
    try:
        package = tomllib.loads(cargo_toml.decode("utf-8"))["package"]
        metadata = package["metadata"]["binstall"]
    except (KeyError, TypeError, UnicodeDecodeError, tomllib.TOMLDecodeError) as error:
        raise MetadataError(f"could not read package.metadata.binstall: {error}") from error

    if not isinstance(metadata, dict):
        raise MetadataError("package.metadata.binstall must be a TOML table")

    if metadata.get("bin-dir") != "{ bin }":
        raise MetadataError('binstall bin-dir must be exactly "{ bin }"')
    if metadata.get("disabled-strategies") != ["quick-install"]:
        raise MetadataError(
            "binstall must disable only quick-install and preserve the compile fallback"
        )

    overrides = metadata.get("overrides")
    if not isinstance(overrides, dict) or set(overrides) != set(TARGETS):
        raise MetadataError(
            f"binstall overrides must cover exactly {sorted(TARGETS)}"
        )
    for target, (package_format, suffix) in TARGETS.items():
        target_metadata = overrides[target]
        if not isinstance(target_metadata, dict):
            raise MetadataError(f"binstall override for {target} must be a TOML table")
        expected_url = f"{URL_PREFIX}{suffix}"
        if target_metadata.get("pkg-url") != expected_url:
            raise MetadataError(
                f"binstall pkg-url for {target} must be {expected_url!r}"
            )
        if target_metadata.get("pkg-fmt") != package_format:
            raise MetadataError(
                f"binstall pkg-fmt for {target} must be {package_format!r}"
            )

    return {
        "schema_version": "hsum.binstall-metadata-contract.v1",
        "passed": True,
        "targets": sorted(TARGETS),
        "quick_install_disabled": True,
        "compile_fallback_preserved": True,
    }


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    repository = pathlib.Path(__file__).resolve().parents[1]
    parser.add_argument(
        "--cargo-toml", type=pathlib.Path, default=repository / "Cargo.toml"
    )
    args = parser.parse_args()
    report = verify(args.cargo_toml.read_bytes())
    print(json.dumps(report, sort_keys=True, separators=(",", ":")))


if __name__ == "__main__":
    main()
