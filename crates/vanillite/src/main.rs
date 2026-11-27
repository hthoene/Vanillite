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

/// Returns freed memory of the allocator to the system.
fn release_memory() {
    #[cfg(target_env = "musl")]
    {
        // Part of the mimalloc library the global allocator links.
        unsafe extern "C" {
            fn mi_collect(force: bool);
        }
        // SAFETY: `mi_collect` may be called from any thread at any time.
        unsafe { mi_collect(true) };
    }
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    // SAFETY: `malloc_trim` may be called from any thread at any time.
    unsafe {
        libc::malloc_trim(0);
    }
}

const USAGE: &str = "\
Usage: vanillite [command] [options]

Commands:
  run              Start the server (default)
  health           Send a status ping to the local server; exit code 0 if it answers
  convert          Convert the world into another format while the server is stopped
  export <target>  Write a Vanilla (Anvil) copy of an own-format world into <target>
  upgrade-world    Rewrite every chunk of an own-format world written by an older
                   Minecraft version in this version (otherwise chunks are upgraded
                   when they are saved)
  version          Print the version

Options of run, health, convert, export and upgrade-world:
  --dir <path>   Server folder with config files and worlds (default: current folder)

Options of convert:
  --to <format>  vanillite or anvil (default: storage.world-format of vanillite.toml).
                 The new files are verified before the old ones are moved into
                 world-format-backup/ (storage.keep-converted-files) or deleted

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
        Some("convert") => convert(args),
        Some("export") => export(args),
        Some("upgrade-world") => upgrade_world(args),
        Some("version" | "--version" | "-V") => {
            println!("{}", version());
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
            let mut builder = ServerBuilder::new(dir);
            builder.version(version());
            builder.memory_release(release_memory);
            let server = match builder.start() {
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

/// `Vanillite <version> (Minecraft <id>, <commit>)`.
fn version() -> String {
    format!(
        "Vanillite {} (Minecraft {}, {})",
        env!("CARGO_PKG_VERSION"),
        vanillite_data::version::VERSION_ID,
        env!("VANILLITE_COMMIT")
    )
}

/// `vanillite convert [--to <format>] [--dir <path>]`.
fn convert(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir = PathBuf::from(".");
    let mut to = None;
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next().as_deref()) {
            ("--dir", Some(path)) => dir = PathBuf::from(path),
            ("--to", Some("vanillite")) => to = Some(vanillite_storage::WorldFormat::Vanillite),
            ("--to", Some("anvil")) => to = Some(vanillite_storage::WorldFormat::Anvil),
            _ => {
                eprintln!("Invalid option {arg:?}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    logger::init();
    match vanillite_server::convert_world(&dir, to) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("Conversion failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `vanillite upgrade-world [--dir <path>]`.
fn upgrade_world(mut args: impl Iterator<Item = String>) -> ExitCode {
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
    match vanillite_server::upgrade_world(&dir) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("Upgrade failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// `vanillite export <target> [--dir <path>]`.
fn export(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut dir = PathBuf::from(".");
    let mut target = None;
    while let Some(arg) = args.next() {
        match (arg.as_str(), target.is_none()) {
            ("--dir", _) => {
                let Some(path) = args.next() else {
                    eprintln!("--dir needs a folder\n\n{USAGE}");
                    return ExitCode::FAILURE;
                };
                dir = PathBuf::from(path);
            }
            (_, true) if !arg.starts_with("--") => target = Some(PathBuf::from(arg)),
            _ => {
                eprintln!("Invalid option {arg:?}\n\n{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }
    let Some(target) = target else {
        eprintln!("export needs a target folder\n\n{USAGE}");
        return ExitCode::FAILURE;
    };
    logger::init();
    match vanillite_server::export_world(&dir, &target) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            log::error!("Export failed: {e}");
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
