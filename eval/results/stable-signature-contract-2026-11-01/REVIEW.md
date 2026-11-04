# Stable signature preparation — November 1

Independent AI review approved the exact-bundle verifier, stable installer,
real-signature tests, CI job and signing runbook. All11 verifier tests and7
installer tests pass using official Minisign0.12 on macOS. The current alpha
installer smoke also passes with the actual release candidate binary.

Review found a real signature-context framing bug in the initial Python
verifier: splitlines() could interpret unsigned control characters as line
boundaries. The retained red regression reproduces it; bounded ASCII bytes,
explicit LF/CRLF framing and exact authenticated context now reject it.
The proposed Bash NUL-suffix concern did not reproduce against actual
Minisign0.12, which rejects the malformed trusted comment; the installer uses
Minisign's own verified-comment output (-Q) to avoid parsing it independently.

All generated signing keys were disposable test keys. No production signing
identity was created. CI installs Ubuntu's Minisign and runs both real-crypto
suites without credentials. Production Apple credentials, offline artifact
signing authority/key distribution, stable workflow integration, native
qualification and final publication checks remain outstanding. Nothing here
is a stable release or an external security audit.
