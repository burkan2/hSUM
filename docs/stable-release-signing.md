# Stable release signing and verification

Status on November 1, 2026: the asset verifier and stable installer authentication
are implemented and tested with disposable keys. Production Apple credentials,
offline Minisign authority, independent key distribution, and stable workflow
integration are outstanding. The existing `release.yml` publishes alpha tags
only. These instructions do not waive performance or final release gates.

## Authority setup

The release owner supplies the production Minisign public key; the private key
stays under separately protected offline custody and never enters Actions,
repository files, logs, or chat. Record the full base64 public key and SHA-256
of its decoded 42 bytes in the signed source, `SECURITY.md`, and crates.io
package metadata. Review key custody, backup access, rotation, revocation and
compromise recovery before signing a stable tag. Do not use the test keys from
the signature tests as release authority.

The signed Git tag uses the configured GPG authority. This is distinct from
the offline artifact signer. `gh release create --verify-tag` checks that the
tag exists remotely; the workflow must separately run `git verify-tag` and
bind the verified tag to the build commit and Cargo version.

Apple signing requires `APPLE_CERTIFICATE_BASE64`, `APPLE_CERTIFICATE_PASSWORD`,
`APPLE_SIGNING_IDENTITY`, `APPLE_ID`, `APPLE_APP_PASSWORD`, `APPLE_TEAM_ID`, and
`KEYCHAIN_PASSWORD` under the protected release environment. Stable builds must
fail if any is absent, require notarization acceptance, and retain verification
evidence. The alpha opt-out is forbidden for stable installation.

## Draft assembly and offline signing

After the exact candidate passes both native qualification protocols and final
review, the stable workflow must build and test both targets, check unsigned
binary reproducibility, sign/notarize macOS, then assemble the final archives.
Render both `@HSUM_VERSION@` and `@HSUM_MINISIGN_PUBLIC_KEY@` in
`scripts/install.sh` before checksumming. Generate the SBOMs, locked dependency
licenses, attestations, archive checksum sidecars and `SHA256SUMS` after the
payload bytes are final. Retain all provenance outside the exact asset bundle.

For a stable tag such as `v0.1.0`, the unsigned draft contains exactly nine
regular files: two archives, their two checksum sidecars, two SPDX SBOMs, the
Cargo license inventory, `install-hsum.sh`, and `SHA256SUMS`. Validate assembly:

```sh
python3 scripts/verify_stable_release.py "$tag" "$dist" \
  --public-key "$independently_verified_public_key" --unsigned-draft
```

This reports `signatures_verified: false`; it is never a publication gate.
The owner reviews and transfers the final payloads to the offline signing
environment. Sign exactly these four files with tag-and-filename context:

```sh
for asset in SHA256SUMS install-hsum.sh \
  "hsum-$tag-aarch64-apple-darwin.zip" \
  "hsum-$tag-x86_64-unknown-linux-gnu.tar.gz"
do
  minisign -S -s "$offline_secret_key_path" -m "$dist/$asset" \
    -t "hSUM $tag: $asset"
done
```

Use prehashed signatures (the default); do not use legacy `-l`. Only the four
`.minisig` files return to the online workflow. Upload them to the existing
draft without replacing payloads. Download all draft assets into an empty
directory and verify the resulting exact 13-file bundle:

```sh
python3 scripts/verify_stable_release.py "$tag" "$downloaded_draft" \
  --public-key "$independently_verified_public_key"
```

Verification authenticates all four signatures, their trusted comments, the
manifest's exact payload coverage, every payload hash, archive sidecars, and
the asset allowlist. It rejects missing/extra files, symlinks and tampering.
The publisher must also verify the draft still targets the verified source,
review clean-machine trials and attestations, and require the protected final
release approval. Only then may it publish. Verify downloaded immutable assets
again after publication and retain the receipts.

## High-assurance installation

Obtain the public-key fingerprint from an independent trusted distribution
channel and verify the full public key before trusting any downloaded file.
Download the installer, `SHA256SUMS`, and both detached signatures without
executing the installer. Authenticate each with `minisign -V -H`, verify its
trusted comment names the intended tag and file, and check the installer's
hash against the authenticated manifest before executing the local script.
The installer then authenticates the manifest and selected native archive
before extraction or execution, and requires Gatekeeper approval on macOS.
The HTTPS convenience path still trusts delivery of the installer itself.

## Development checks

`scripts/test_verify_stable_release.py` and `scripts/test_stable_installer.py`
use a real Minisign executable and disposable keys deleted after each test.
Set `HSUM_TEST_MINISIGN` to an absolute executable path when it is not on PATH.
They exercise valid signatures, context substitution, tampering, malformed
signature framing, wrong authority, extra assets, alpha-bypass refusal and
Gatekeeper failure. `scripts/installer-smoke.sh` continues to cover the alpha
installer with the actual candidate binary. CI runs the stable signature tests
in a separate Linux job.
