#!/usr/bin/env python3
"""Verify the exact stable asset/checksum contract and offline signatures."""

import argparse
import base64
import binascii
import hashlib
import json
from pathlib import Path
import re
import stat
import subprocess
import sys


class ReleaseError(ValueError):
    pass


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def bounded_text(path, maximum):
    if path.stat().st_size > maximum:
        raise ReleaseError(f"oversized release metadata: {path.name}")
    # Preserve exact line framing: universal-newline translation would turn
    # unsigned carriage returns into apparent trusted-comment boundaries.
    return path.read_bytes().decode("ascii")


def checksum_entries(text):
    entries = {}
    for line in text.splitlines():
        match = re.fullmatch(r"([0-9a-f]{64})  ([A-Za-z0-9_.-]+)", line)
        if not match or match[2] in entries:
            raise ReleaseError("malformed or duplicate checksum entry")
        entries[match[2]] = match[1]
    return entries


def verify(tag, dist, public_key, *, minisign="minisign", unsigned_draft=False):
    if not re.fullmatch(r"v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)", tag):
        raise ReleaseError("stable tag must be an exact canonical vX.Y.Z version")
    # The caller must obtain this key from the signed source or independent
    # distribution channel, never from the bundle being verified.
    try:
        decoded = base64.b64decode(public_key, validate=True)
    except (ValueError, binascii.Error) as error:
        raise ReleaseError("invalid pinned Minisign public key") from error
    if len(decoded) != 42 or decoded[:2] != b"Ed":
        raise ReleaseError("missing or invalid pinned Minisign public key")
    dist = Path(dist)
    archives = [f"hsum-{tag}-aarch64-apple-darwin.zip", f"hsum-{tag}-x86_64-unknown-linux-gnu.tar.gz"]
    payloads = set(archives + [f"hsum-{tag}-aarch64-apple-darwin.spdx.json",
                               f"hsum-{tag}-x86_64-unknown-linux-gnu.spdx.json",
                               f"hsum-{tag}-cargo-licenses.json", "install-hsum.sh"])
    signed = ["SHA256SUMS", "install-hsum.sh", *archives]
    expected = payloads | {"SHA256SUMS"} | {name + ".sha256" for name in archives}
    if not unsigned_draft:
        expected |= {name + ".minisig" for name in signed}
    actual = list(dist.iterdir())
    if {path.name for path in actual} != expected or any(
        not stat.S_ISREG(path.lstat().st_mode) for path in actual
    ):
        raise ReleaseError("stable assets must be exactly the expected regular files")
    if not unsigned_draft:
        for name in signed:
            signature = dist / (name + ".minisig")
            text = bounded_text(signature, 4096)
            normalized = text.replace("\r\n", "\n")
            if any(ord(char) < 32 and char != "\n" for char in normalized):
                raise ReleaseError(f"invalid signature comment framing: {name}")
            lines = normalized.removesuffix("\n").split("\n")
            if len(lines) != 4:
                raise ReleaseError(f"invalid signature line count: {name}")
            try:
                result = subprocess.run(
                    [minisign, "-V", "-H", "-m", str(dist / name), "-x", str(signature), "-P", public_key],
                    capture_output=True, text=True, timeout=60, check=False,
                )
            except (OSError, subprocess.TimeoutExpired) as error:
                raise ReleaseError("Minisign verification could not run") from error
            if result.returncode != 0:
                raise ReleaseError(f"invalid signature: {name}")
            if lines[2] != f"trusted comment: hSUM {tag}: {name}":
                raise ReleaseError(f"signature is not bound to this tag and filename: {name}")
    entries = checksum_entries(bounded_text(dist / "SHA256SUMS", 16 * 1024))
    if set(entries) != payloads:
        raise ReleaseError("SHA256SUMS must cover every payload exactly once")
    for name, expected_digest in entries.items():
        if digest(dist / name) != expected_digest:
            raise ReleaseError(f"checksum mismatch: {name}")
    for archive in archives:
        sidecar = checksum_entries(bounded_text(dist / (archive + ".sha256"), 1024))
        if sidecar != {archive: entries[archive]}:
            raise ReleaseError(f"invalid archive checksum sidecar: {archive}")
    return {"tag": tag, "payload_files": len(payloads),
            "signatures_verified": not unsigned_draft,
            "signed_files": 0 if unsigned_draft else len(signed),
            "public_key_sha256": hashlib.sha256(decoded).hexdigest(),
            "sha256sums_sha256": digest(dist / "SHA256SUMS")}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("tag")
    parser.add_argument("dist", type=Path)
    parser.add_argument("--public-key", required=True, help="Base64 key from the signed source/independent channel")
    parser.add_argument("--unsigned-draft", action="store_true", help="Check payload assembly only; never sufficient for publication")
    args = parser.parse_args()
    try:
        report = verify(args.tag, args.dist, args.public_key, unsigned_draft=args.unsigned_draft)
    except (ReleaseError, OSError, UnicodeError) as error:
        print(f"stable release verification failed: {error}", file=sys.stderr)
        return 1
    print(json.dumps(report, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
