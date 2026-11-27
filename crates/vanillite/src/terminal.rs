//! Questions at the console on the first start, and keeping a Windows console window open after a
//! failed start so its message stays readable.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::Path;

/// Asks at the console whether the user accepts the Minecraft EULA. Only when a person sits at
/// the terminal: never in a container (Docker, Pterodactyl give a TTY nobody may watch), with
/// piped stdin or under systemd, so those never wait.
pub fn accept_eula(dir: &Path) -> bool {
    if !interactive() {
        return false;
    }
    print!(
        "Do you accept the Minecraft EULA (https://aka.ms/MinecraftEULA)? Type 'yes' to accept: "
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().lock().read_line(&mut answer).is_err()
        || !answer.trim().eq_ignore_ascii_case("yes")
    {
        return false;
    }
    match vanillite_server::eula::accept(dir) {
        Ok(()) => {
            log::info!(
                "EULA accepted, saved in {}",
                vanillite_server::eula::file(dir).display()
            );
            true
        }
        Err(e) => {
            log::error!(
                "Cannot write {}: {e}",
                vanillite_server::eula::file(dir).display()
            );
            false
        }
    }
}

/// Whether a person can answer at the console.
fn interactive() -> bool {
    let container = std::env::var_os("P_SERVER_UUID").is_some()
        || Path::new("/.dockerenv").exists()
        || Path::new("/run/.containerenv").exists();
    !container && std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

/// After a failed start: when the console window belongs to this process alone (started with a
/// double click), waits for Enter so the window does not close before the message is read.
#[cfg(windows)]
pub fn hold_window() {
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    }
    let mut ids = [0u32; 2];
    // SAFETY: `ids` has room for the 2 entries the call may write.
    let count = unsafe { GetConsoleProcessList(ids.as_mut_ptr(), 2) };
    if count == 1 && std::io::stdin().is_terminal() {
        print!("Press Enter to close this window");
        let _ = std::io::stdout().flush();
        let _ = std::io::stdin().lock().read_line(&mut String::new());
    }
}

/// Only Windows closes a console window with its process.
#[cfg(not(windows))]
pub fn hold_window() {}
