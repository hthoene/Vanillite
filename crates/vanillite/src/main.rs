//! The Vanillite server binary: a thin CLI over `vanillite-server`.

mod health;
mod logger;

use std::io::BufRead as _;
use std::path::PathBuf;
use std::process::ExitCode;

use vanillite_server::ServerBuilder;

// musl's allocator is slow with many threads; the static (Docker) build uses mimalloc.
#[cfg(target_env = "musl")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

const USAGE: &str = "\
Usage: vanillite [command] [options]

Commands:
  run        Start the server (default)
  health     Send a status ping to the local server; exit code 0 if it answers
  version    Print the version

Options of run and health:
  --dir <path>   Server folder with config files and worlds (default: current folder)

Options of health:
  --port <port>  Port to ping (default: VANILLITE_SERVER_PORT, else server-port of
                 server.properties, else 25565)

Environment:
  VANILLITE_LOG=<level>   error, warn, info (default), debug or trace
  VANILLITE_<KEY>=<value> Overrides a key of vanillite.toml or server.properties
                          (e.g. VANILLITE_SERVER_PORT, VANILLITE_MOTD)";

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let command = args.next();
    match command.as_deref() {
        None | Some("run") => run(args),
        Some("health") => health::run(args, USAGE),
        Some("version" | "--version" | "-V") => {
            println!("vanillite {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("help" | "--help" | "-h") => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("Unknown command {other:?}\n\n{USAGE}");
            ExitCode::FAILURE
        }
    }
}

fn run(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir = PathBuf::from(".");
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--dir", Some(path)) => dir = PathBuf::from(path),
            _ => {
                eprintln!("Invalid option {arg:?}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    logger::init();

    // The tick loop runs on a thread named like Vanilla's, so log lines show "Server thread".
    let server_thread = std::thread::Builder::new()
        .name("Server thread".to_owned())
        .spawn(move || {
            let server = match ServerBuilder::new(dir).start() {
                Ok(server) => server,
                Err(e) => {
                    log::error!("Failed to start the server: {e}");
                    return false;
                }
            };
            let handle = server.handle();
            let signal_handle = handle.clone();
            if let Err(e) = ctrlc::set_handler(move || signal_handle.stop()) {
                log::warn!("Cannot handle Ctrl-C and SIGTERM: {e}");
            }
            spawn_console(handle);
            server.run();
            true
        });
    match server_thread.map(|thread| thread.join()) {
        Ok(Ok(true)) => ExitCode::SUCCESS,
        Ok(Ok(false)) => ExitCode::FAILURE,
        Ok(Err(_)) => {
            log::error!("The server thread crashed");
            ExitCode::FAILURE
        }
        Err(e) => {
            log::error!("Cannot start the server thread: {e}");
            ExitCode::FAILURE
        }
    }
}

/// Reads console commands from stdin. The thread ends with stdin (e.g. in Docker without a
/// terminal); the server keeps running.
fn spawn_console(handle: vanillite_server::ServerHandle) {
    let spawned = std::thread::Builder::new()
        .name("Server console handler".to_owned())
        .spawn(move || {
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(line) => handle.console(line),
                    Err(_) => return,
                }
            }
        });
    if let Err(e) = spawned {
        log::warn!("Cannot start the console reader: {e}");
    }
}
