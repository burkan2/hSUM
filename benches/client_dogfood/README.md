# Cross-client dogfood qualification

This protocol runs one checksum-pinned hSUM candidate through three real MCP
client paths:

1. a minimal generic JSON-RPC stdio client;
2. the exact Codex CLI version frozen in `manifest.json`; and
3. the exact Claude Code CLI version frozen in `manifest.json`.

Every client must call `evidence_search`, pass the returned citation to
`evidence_get`, and observe the fixture token in hSUM's immutable bytes. The
agent clients are accepted from their machine-readable tool event streams, not
from their final prose alone.

The harness does not install an MCP server or edit durable client state. Codex
receives per-invocation TOML overrides, runs ephemerally in a read-only sandbox,
and continues to use its existing authentication. Claude Code receives a
temporary strict MCP JSON file, an explicit two-tool allowlist, a spend cap,
temporary-project-only settings, and `--no-session-persistence`.

Validate the frozen protocol without calling either model:

```bash
python3 benches/client_dogfood/harness.py validate
python3 benches/client_dogfood/test_harness.py
```

Run the live protocol against a candidate (this invokes the two authenticated
agent clients and may incur model usage):

```bash
python3 benches/client_dogfood/harness.py run \
  --hsum "$PWD/target/release/hsum" \
  --codex /Applications/ChatGPT.app/Contents/Resources/codex \
  --claude /Users/b.k./.local/bin/claude \
  --output /absolute/evidence/client-dogfood.json
```

The report records exact executable hashes, versions, tool-event acceptance,
candidate identity, and elapsed time. Raw JSONL/stdout and stderr are written
beside it for audit, but they are not promoted as product telemetry.
