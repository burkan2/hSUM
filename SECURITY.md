# Security policy

## Supported releases

Security fixes target the latest published hSUM alpha release. Alpha releases
are intentionally narrow: macOS arm64 and Linux x86_64 on local filesystems.
Windows and every unlisted platform are unsupported.

## Report a vulnerability

Use the **Report a vulnerability** button in this repository's Security tab to
open a private report. Do not file a public issue for a suspected
vulnerability.

Reports should include the affected hSUM version, operating system and
architecture, minimal reproduction steps, expected and actual behaviour, and
whether source files or index data could be disclosed, modified, or made
unavailable. Do not attach private source material unless a maintainer has
provided a secure handling path.

## Response target

Maintainers aim to acknowledge valid reports within three business days and to
provide a status update at least every seven calendar days. Fix timing depends
on impact and reproducibility. We will credit reporters only with their
permission.

## Release compromise

If a release artifact, signing credential, or release workflow is suspected to
be compromised, maintainers will immediately pause new releases, revoke the
affected credentials, mark the affected GitHub Release as compromised, publish
a replacement release with new checksums and provenance, and explain the scope
and remediation in that release's notes.
