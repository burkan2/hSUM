# Contributing to hSUM

hSUM is made by the creator of ArgonGate, Burkan Alp Kale, and released as open source
under the dual `MIT OR Apache-2.0` license. Contributions are welcome.

## Licensing of contributions

By opening a pull request you agree that your contribution is licensed under
the same dual `MIT OR Apache-2.0` terms as the rest of the project, unless you
state otherwise. No separate agreement or sign-off is required.

## Before you open a pull request

Run the repository's local check gate; it must pass:

```bash
cargo xtask check
```

This runs formatting, Clippy with warnings denied, all tests, and doctests.
It also regenerates the implementation-derived reference set in memory and
fails when the checked-in pages or their local links are stale. After an
intentional CLI, MCP, error, configuration, JSONL, API, or managed-path change,
update that set first:

```bash
cargo xtask references
cargo xtask references --check
```

The networked `cargo xtask references --check-remote` audit is a release and
documentation-deployment check. It requires every versioned public error page
to return successfully and identify its own subcode; ordinary contributor
checks remain deterministic and offline-capable.

## Scope

hSUM is an evidence engine, not an answer engine. Contributions must preserve
the project's core invariants: immutable citations, project-bound read-only
MCP, deterministic retrieval, and local-only operation with no network,
account, or telemetry path. Larger changes are best discussed in an issue
before implementation.
