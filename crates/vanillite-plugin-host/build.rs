//! Records the target triple: plugin libraries built for another target are refused.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=VANILLITE_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
