# 30-second source-drift video

## Hook: 0–3 seconds

Split screen, very large text:

> **The file says 9. What did your agent see before the edit?**

## Establish the evidence: 3–8 seconds

```text
pub const RETRY_LIMIT: u32 = 3;
```

Run hSUM search. Flash the citation, highlighting `rev=` and `#bytes=`.

## Break the live source: 8–12 seconds

Change `3` to `9`. Do not commit either version.

```text
pub const RETRY_LIMIT: u32 = 9;
```

## Same question, two conditions: 12–22 seconds

Left:

```text
WITHOUT hSUM
current: 9
previous: unknown
```

Right:

```text
WITH hSUM
previous: 3
current: 9
source: CHANGED
```

Animate the old exact passage appearing from `hsum get`.

## Payoff: 22–27 seconds

Use `drift-dashboard.svg` full-screen:

> **Search finds what is there. hSUM proves what was there.**

## Close: 27–30 seconds

> **Keep the source. Return the passage.**
>
> `github.com/burkan2/hSUM`

Small footer: “Local, uncommitted fixture. Reproducible benchmark. No LLM judge.”

## Follow-up carousel/video

Do not use the superseded 12-task comparison as a follow-up claim. The frozen
25-query development baseline does not show an overall hSUM retrieval win.

1. Frame 1: “25 frozen tasks · 5 balanced query classes · graded labels.”
2. Frame 2: “3 fresh indexes · same frozen corpus · every range published.”
3. Frame 3: “Mean nDCG@5: hSUM 26.3% · git grep 22.5% · ripgrep 22.2%.”
4. Frame 4: “hSUM rebuild range: 22.9–28.3%. We publish the weakness.”
5. Frame 5: “Median: 153.6 ms hSUM · 15.8 ms git grep · 6.9 ms ripgrep.”
6. Frame 6: “Paraphrase and multi-evidence: 0%. That is the roadmap.”
7. Frame 7: link to the frozen manifest, all three raw builds, and validation command.

Every frame must say “hSUM repository development baseline” or show that scope
in a readable footer. The source-drift video above is the stronger viral asset
because it demonstrates a unique property with an objective oracle instead of
implying an overall retrieval-quality lead.
