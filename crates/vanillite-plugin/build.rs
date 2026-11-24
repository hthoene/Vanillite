//! Records the target triple, which the host compares with its own before loading a plugin.

fn main() {
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=VANILLITE_PLUGIN_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
