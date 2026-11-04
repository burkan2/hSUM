# Corrected Linux lexical diagnostic — b8b843e

**Valid reduced A/B/A; not qualification.** Isolated build directories produce
baseline SHA 21be5500b059bfa01d62278bdec4bf8ba2c0ab4d21a65de36ee9015aedba5218
and candidate SHA bc033578bbb8e8f513bf8ec298622337e9b8f4aab8f858e712c77becbdba33ed.
All 684 call-result hashes match retained baseline arrays. There are 190 measured
observations per phase (ten passes of 19 lexical queries), plus two warmups.
All complete result fields and explanations agree across the three phases.

ordinary-02 median:81 /79 /80ms; p95:83 /80 /83ms. The joined-docsize change
has a small within-run gain. This runner is faster than the prior runner;
cross-run timing differences cannot be attributed to the optimization.
Fresh lexical preparation contains100,000 passages, no inference, followed
by explicit offline checkpointing. No cold-cache qualification was performed.
The original invalid comparison 41304545751 remains retained separately.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41319532732
