/// Re-export Cargo's build-target triple so `hsum --version --verbose` can
/// report the compiled target without a runtime guess.
fn main() {
    verify_sqlite_position_source();
    // Compile with the same target C toolchain/profile as bundled SQLite. This
    // retains its floating-point contraction behavior in BM25 arithmetic.
    println!("cargo:rerun-if-changed=src/search/bm25_math.c");
    cc::Build::new()
        .file("src/search/bm25_math.c")
        .compile("hsum_bm25_math");
    let target = std::env::var("TARGET")
        .expect("Cargo always provides TARGET to build scripts (see the build-script reference)");
    println!("cargo:rustc-env=HSUM_TARGET_TRIPLE={target}");
}

fn verify_sqlite_position_source() {
    use sha2::{Digest, Sha256};
    use std::{env, fs, path::PathBuf};

    println!("cargo:rustc-check-cfg=cfg(hsum_verified_sqlite_positions)");
    println!("cargo:rerun-if-env-changed=LIBSQLITE3_SYS_USE_PKG_CONFIG");
    println!("cargo:rerun-if-env-changed=LIBSQLITE3_FLAGS");
    // libsqlite3-sys emits INCLUDE and LIB_DIR only from its bundled build.
    // Linked/system, Gecko and loadable-extension builds do not qualify.
    if env::var_os("LIBSQLITE3_SYS_USE_PKG_CONFIG").is_some_and(|value| value != "0")
        || env::var_os("LIBSQLITE3_FLAGS").is_some()
        || env::var_os("DEP_SQLITE3_LINK_TARGET").is_some()
        || env::var_os("DEP_SQLITE3_LIB_DIR").is_none()
    {
        return;
    }
    let Some(include) = env::var_os("DEP_SQLITE3_INCLUDE") else {
        return;
    };
    let source = PathBuf::from(include).join("sqlite3.c");
    println!("cargo:rerun-if-changed={}", source.display());
    let Ok(bytes) = fs::read(source) else {
        return;
    };
    // Intentional allowlist: a dependency update falls back to public FTS5
    // iteration until its allocation and position-list invariants are audited.
    if Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>()
        == "9512509b1bccb7461f79bea8aad6280ae4699e925fa4804381b71f59e7efb0c5"
    {
        println!("cargo:rustc-cfg=hsum_verified_sqlite_positions");
    }
}
