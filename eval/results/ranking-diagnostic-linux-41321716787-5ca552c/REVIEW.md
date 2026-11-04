# Linux ranking diagnostic — `5ca552c`

This A/B/A diagnostic is **not qualification**. Baseline `41af5c7`, candidate
`5ca552c`, then baseline again ran on one fixture within one workflow. Ordinary
lexical medians were 137/122/138 ms, means 138.9/122.2/137.4 ms and p95 values
155/124/139 ms. The candidate's mean is about 11.5% lower than the surrounding
baseline mean. These data do not establish a cross-platform SLO pass.

Independent AI review checked all 684 response digests, 570 observations and 57
summary records. Baseline and candidate binaries are distinct: baseline SHA-256
`21be5500b059bfa01d62278bdec4bf8ba2c0ab4d21a65de36ee9015aedba5218`, candidate
`6b922d22c578e1d76a02ac279abb77356ffb5ad004b86fbb9fba5832681e025c`.
All retained result digests agree. Hosted baseline timings vary between runs;
do not compare this run's absolute values with another runner's baseline.
The subsequent full candidate qualification still failed ordinary lexical SLOs.

Workflow: https://github.com/burkan2/hSUM/actions/runs/41321716787
