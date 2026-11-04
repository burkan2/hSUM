#!/usr/bin/env python3
"""Require matching offline artifact-signer pins in the signed source."""

import argparse
import base64
import binascii
import hashlib
import json
from pathlib import Path
import re
import stat
import sys
import tomllib


class AuthorityError(ValueError):
    pass


def read_regular(path, maximum):
    try:
        metadata = path.lstat()
        if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > maximum:
            raise AuthorityError(f"invalid authority file: {path.name}")
        return path.read_text(encoding="utf-8")
    except (OSError, UnicodeError) as error:
        raise AuthorityError(f"missing or unreadable authority file: {path.name}") from error


def verify(root):
    root = Path(root)
    text = read_regular(root / "release/minisign.pub", 4096)
    if any(ord(char) < 32 and char != "\n" for char in text):
        raise AuthorityError("invalid public-key file framing")
    lines = text.removesuffix("\n").split("\n")
    if len(lines) != 2 or not lines[0].startswith("untrusted comment: "):
        raise AuthorityError("expected a two-line Minisign public-key file")
    key = lines[1]
    try:
        decoded = base64.b64decode(key, validate=True)
    except (ValueError, binascii.Error) as error:
        raise AuthorityError("invalid Minisign public key") from error
    if len(decoded) != 42 or decoded[:2] != b"Ed":
        raise AuthorityError("invalid Minisign public-key algorithm or length")
    fingerprint = hashlib.sha256(decoded).hexdigest()
    try:
        cargo = tomllib.loads(read_regular(root / "Cargo.toml", 1024 * 1024))
        pins = cargo["package"]["metadata"]["hsum-release"]
    except (KeyError, TypeError, tomllib.TOMLDecodeError) as error:
        raise AuthorityError("missing stable signing pins in Cargo metadata") from error
    if not isinstance(pins, dict):
        raise AuthorityError("invalid stable signing pins in Cargo metadata")
    if pins.get("minisign-public-key") != key or pins.get("minisign-public-key-sha256") != fingerprint:
        raise AuthorityError("Cargo metadata does not match the artifact-signing key")
    security = read_regular(root / "SECURITY.md", 1024 * 1024)
    recorded = re.findall(r"^Minisign public key SHA-256: `([0-9a-f]{64})`$", security, re.MULTILINE)
    if recorded != [fingerprint]:
        raise AuthorityError("SECURITY.md must contain exactly one matching public-key fingerprint")
    return {"public_key": key, "public_key_sha256": fingerprint}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("root", type=Path, nargs="?", default=Path("."))
    args = parser.parse_args()
    try:
        print(json.dumps(verify(args.root), sort_keys=True))
    except AuthorityError as error:
        print(f"stable authority verification failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
