# Invalid Linux A/B/A comparison — run 41304545751

**Disposition: INVALID COMPARISON.** All three phases record the same binary
SHA-256: `21be5500b059bfa01d62278bdec4bf8ba2c0ab4d21a65de36ee9015aedba5218`.
Independent review detected this after download. The baseline build reused the
candidate Cargo target directory; Cargo's existing package fingerprints then
allowed the candidate step to retain the baseline output. This run cannot
establish either a performance gain or the absence of one between41af5c7 and
22b6a55. Its complete raw report and log remain unchanged.

Observed ordinary-02 medians were 107 / 107 / 108 ms and p95s113 / 108 / 108 ms.
These are repeated measurements of one binary, not valid A/B evidence. This
lexical-only diagnostic also has no fresh inference/cold-cache qualification.

Repair: isolate baseline and candidate target directories; reject equal binary
hashes before fixture preparation or measurement. The failing-before and
passing-after regression is retained alongside this report. A corrected run
is required. No frozen candidate41af5c7 qualification evidence is affected.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41304545751
