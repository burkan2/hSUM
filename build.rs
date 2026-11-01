/// Re-export Cargo's build-target triple so `hsum --version --verbose` can
/// report the compiled target without a runtime guess.
fn main() {
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
