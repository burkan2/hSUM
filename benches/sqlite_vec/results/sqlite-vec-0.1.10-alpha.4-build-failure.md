# sqlite-vec 0.1.10-alpha.4 crates.io build failure

Observed on 2026-08-02 with Rust 1.91.0 on macOS arm64 while compiling the exact
crates.io package as an optional hSUM dependency.

Command:

```text
cargo +1.91.0 test --features vector-portability --example sqlite-vec-portability
```

Relevant compiler output:

```text
Compiling sqlite-vec v0.1.10-alpha.4
sqlite-vec.c:3772:10: fatal error: 'sqlite-vec-diskann.c' file not found
#include "sqlite-vec-diskann.c"
         ^~~~~~~~~~~~~~~~~~~~~~
error: failed to run custom build command for `sqlite-vec v0.1.10-alpha.4`
```

The package's `sqlite-vec.c` defines `SQLITE_VEC_ENABLE_DISKANN` to `1` by
default and includes that C file, while the downloaded crate contains only the
amalgamation, headers, Rust binding, and build script. hSUM does not patch or
vendor the missing third-party file. The spike uses pinned non-alpha
`sqlite-vec 0.1.7` and records the behavioral differences separately.
