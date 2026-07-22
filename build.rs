/// Re-export Cargo's build-target triple so `hsum --version --verbose` can
/// report the compiled target without a runtime guess.
fn main() {
    let target = std::env::var("TARGET")
        .expect("Cargo always provides TARGET to build scripts (see the build-script reference)");
    println!("cargo:rustc-env=HSUM_TARGET_TRIPLE={target}");
}
