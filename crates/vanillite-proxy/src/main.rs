//! The `vanillite-proxy` binary.

use std::path::PathBuf;
use std::process::ExitCode;

const USAGE: &str = "\
Usage: vanillite-proxy [--dir <path>]

Reads proxy.toml (written with defaults if missing) and the forwarding secret from <path>
(default: current folder).

Environment:
  VANILLITE_LOG=<level>         error, warn, info (default), debug or trace
  VANILLITE_PROXY_SECRET=<s>    forwarding secret instead of the secret file";

fn main() -> ExitCode {
    let mut dir = PathBuf::from(".");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--dir", Some(path)) => dir = PathBuf::from(path),
            _ => {
                eprintln!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    env_logger::Builder::from_env(env_logger::Env::new().filter_or("VANILLITE_LOG", "info")).init();
    let config = match vanillite_proxy::ProxyConfig::load(&dir) {
        Ok(c) => c,
        Err(e) => {
            log::error!("{e}");
            return ExitCode::FAILURE;
        }
    };
    let proxy = match vanillite_proxy::Proxy::start(config) {
        Ok(p) => p,
        Err(e) => {
            log::error!("cannot start: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (stop_tx, stop_rx) = std::sync::mpsc::channel();
    if let Err(e) = ctrlc::set_handler(move || {
        let _ = stop_tx.send(());
    }) {
        log::warn!("cannot install the stop handler: {e}");
    }
    let _ = stop_rx.recv();
    log::info!("Stopping");
    proxy.shutdown();
    ExitCode::SUCCESS
}
