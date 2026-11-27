//! CPU level dispatch: the Linux x86-64 release archives and the Docker image ship the baseline
//! build (`vanillite`, runs on every x86-64 CPU) and an x86-64-v3 build (`vanillite-x86-64-v3`,
//! AVX2/BMI2/FMA, bit-identical results, fewer instructions). The baseline build starts the v3
//! build in its place when the CPU has its features; `VANILLITE_CPU=baseline` turns that off.

/// The CPU level this binary was built for.
pub fn level() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_feature = "avx2")) {
        "x86-64-v3"
    } else if cfg!(target_arch = "x86_64") {
        "x86-64"
    } else {
        std::env::consts::ARCH
    }
}

/// Replaces this process with the x86-64-v3 build when it lies next to this binary, the CPU
/// supports it and `VANILLITE_CPU` is not `baseline`. Returns why it did not (for the log);
/// `None` where there is nothing to choose.
#[cfg(all(
    target_os = "linux",
    target_arch = "x86_64",
    not(target_feature = "avx2")
))]
pub fn dispatch() -> Option<String> {
    use std::os::unix::process::CommandExt as _;

    /// File name of the x86-64-v3 build next to the baseline binary.
    const V3_BINARY: &str = "vanillite-x86-64-v3";

    let exe = std::env::current_exe().ok()?;
    let v3 = exe.with_file_name(V3_BINARY);
    if exe.file_name().is_some_and(|n| n == V3_BINARY) || !v3.is_file() {
        return None;
    }
    if std::env::var("VANILLITE_CPU").is_ok_and(|v| v == "baseline") {
        return Some("x86-64-v3 build present, VANILLITE_CPU=baseline".to_owned());
    }
    // The x86-64-v3 level (see the psABI): what `-C target-cpu=x86-64-v3` lets the compiler use.
    let supported = std::arch::is_x86_feature_detected!("avx2")
        && std::arch::is_x86_feature_detected!("bmi1")
        && std::arch::is_x86_feature_detected!("bmi2")
        && std::arch::is_x86_feature_detected!("fma")
        && std::arch::is_x86_feature_detected!("f16c")
        && std::arch::is_x86_feature_detected!("lzcnt")
        && std::arch::is_x86_feature_detected!("movbe");
    if !supported {
        return Some("the CPU lacks x86-64-v3 features".to_owned());
    }
    let mut args = std::env::args_os();
    let arg0 = args.next().unwrap_or_else(|| v3.clone().into_os_string());
    let err = std::process::Command::new(&v3).arg0(arg0).args(args).exec();
    Some(format!("starting {} failed: {err}", v3.display()))
}

#[cfg(not(all(
    target_os = "linux",
    target_arch = "x86_64",
    not(target_feature = "avx2")
)))]
pub fn dispatch() -> Option<String> {
    None
}
