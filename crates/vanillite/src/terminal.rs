//! The server console: the command line (a line editor with a `> ` prompt when a person sits at
//! the terminal, else plain stdin lines), the EULA question on the first start, colours on
//! Windows consoles, and keeping a Windows console window open after a failed start.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, PoisonError, mpsc};

use vanillite_server::ServerHandle;

/// Whether the console thread waits in the line editor.
static EDITING: AtomicBool = AtomicBool::new(false);
/// The terminal settings before the line editor, restored on exit.
static SAVED: Mutex<Option<Mode>> = Mutex::new(None);

/// Reads console commands. With a person at the terminal: a line editor with a `> ` prompt,
/// history, and log lines printed above the input line. Otherwise (Docker, Pterodactyl, systemd,
/// pipes) plain stdin lines; that thread ends with stdin, the server keeps running.
pub fn spawn_console(handle: ServerHandle) {
    let (ready, editor) = mpsc::channel();
    let interactive = interactive();
    let spawned = std::thread::Builder::new()
        .name("Server console handler".to_owned())
        .spawn(move || {
            if interactive {
                // The editor is not `Send`: it lives on this thread, its printer goes back.
                let saved = Mode::save();
                let config = rustyline::Config::builder()
                    .auto_add_history(true)
                    .max_history_size(1000)
                    .map(rustyline::config::Builder::build);
                let editor = config.and_then(rustyline::DefaultEditor::with_config);
                match editor.and_then(|mut rl| Ok((rl.create_external_printer()?, rl))) {
                    Ok((printer, rl)) => {
                        *SAVED.lock().unwrap_or_else(PoisonError::into_inner) = saved;
                        crate::logger::set_printer(Box::new(printer));
                        let _ = ready.send(None);
                        return edit(rl, &handle);
                    }
                    Err(e) => {
                        let _ = ready.send(Some(e));
                    }
                }
            } else {
                let _ = ready.send(None);
            }
            for line in std::io::stdin().lock().lines() {
                match line {
                    Ok(line) => handle.console(line),
                    Err(_) => return,
                }
            }
        });
    match spawned {
        Ok(_) => {
            if let Ok(Some(e)) = editor.recv() {
                log::warn!("Cannot start the console line editor, reading plain lines: {e}");
            }
        }
        Err(e) => log::warn!("Cannot start the console reader: {e}"),
    }
}

/// The line editor loop. Ctrl+C and Ctrl+D stop the server like `stop`; the prompt stays until
/// the process ends.
fn edit(mut rl: rustyline::DefaultEditor, handle: &ServerHandle) {
    use rustyline::error::ReadlineError;
    EDITING.store(true, Ordering::SeqCst);
    loop {
        match rl.readline("> ") {
            Ok(line) => handle.console(line),
            Err(ReadlineError::Interrupted | ReadlineError::Eof) => handle.stop(),
            Err(e) => {
                EDITING.store(false, Ordering::SeqCst);
                log::warn!("The console stopped reading: {e}");
                return;
            }
        }
    }
}

/// Before the process ends: prints what the line editor still holds, restores the terminal and
/// removes the prompt.
pub fn finish() {
    let Some(mut printer) = crate::logger::take_printer() else {
        return;
    };
    let vt = cfg!(not(windows)) || std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty());
    if EDITING.load(Ordering::SeqCst) {
        // The editor takes one line at a time: once the second empty print is taken, the last
        // log line is on screen. Each print ends with a line feed; the cursor-up undoes it.
        let up = if vt { "\x1b[A" } else { "" };
        let _ = printer.print(up.to_owned());
        let _ = printer.print(up.to_owned());
    }
    if let Some(mode) = SAVED.lock().unwrap_or_else(PoisonError::into_inner).take() {
        mode.restore();
    }
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(if vt { b"\r\x1b[2K" } else { b"\r\n" });
    let _ = out.flush();
}

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

/// The terminal settings of stdin (Unix).
#[cfg(unix)]
struct Mode(libc::termios);

#[cfg(unix)]
impl Mode {
    fn save() -> Option<Mode> {
        let mut termios = std::mem::MaybeUninit::<libc::termios>::uninit();
        // SAFETY: `tcgetattr` fills the termios it points to when it returns 0.
        unsafe {
            (libc::tcgetattr(libc::STDIN_FILENO, termios.as_mut_ptr()) == 0)
                .then(|| Mode(termios.assume_init()))
        }
    }

    fn restore(&self) {
        // SAFETY: `self.0` is a termios `tcgetattr` filled.
        unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0) };
    }
}

/// The console modes of stdin and stdout (Windows).
#[cfg(windows)]
struct Mode(u32, u32);

#[cfg(windows)]
impl Mode {
    fn save() -> Option<Mode> {
        Some(Mode(
            kernel32::mode(kernel32::STD_INPUT_HANDLE)?,
            kernel32::mode(kernel32::STD_OUTPUT_HANDLE)?,
        ))
    }

    fn restore(&self) {
        kernel32::set_mode(kernel32::STD_INPUT_HANDLE, self.0);
        kernel32::set_mode(kernel32::STD_OUTPUT_HANDLE, self.1);
    }
}

/// Switches the console to ANSI escape codes (Windows' VT mode). Whether stderr shows them.
#[cfg(windows)]
pub fn enable_vt() -> bool {
    const VT: u32 = 0x0004; // ENABLE_VIRTUAL_TERMINAL_PROCESSING
    if let Some(mode) = kernel32::mode(kernel32::STD_OUTPUT_HANDLE) {
        kernel32::set_mode(kernel32::STD_OUTPUT_HANDLE, mode | VT);
    }
    kernel32::mode(kernel32::STD_ERROR_HANDLE)
        .is_some_and(|mode| kernel32::set_mode(kernel32::STD_ERROR_HANDLE, mode | VT))
}

/// Terminals outside Windows understand ANSI escape codes.
#[cfg(not(windows))]
pub fn enable_vt() -> bool {
    true
}

/// The few console functions of kernel32 the console needs.
#[cfg(windows)]
mod kernel32 {
    use std::ffi::c_void;

    /// `(DWORD)-10`, `-11`, `-12`.
    pub const STD_INPUT_HANDLE: u32 = 0xFFFF_FFF6;
    pub const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5;
    pub const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetStdHandle(which: u32) -> *mut c_void;
        fn GetConsoleMode(console: *mut c_void, mode: *mut u32) -> i32;
        fn SetConsoleMode(console: *mut c_void, mode: u32) -> i32;
        pub fn GetConsoleProcessList(list: *mut u32, count: u32) -> u32;
    }

    /// The console mode of a standard handle; `None` if it is no console.
    pub fn mode(which: u32) -> Option<u32> {
        let mut mode = 0;
        // SAFETY: `GetStdHandle` takes any value; `GetConsoleMode` writes one u32 to `mode` and
        // fails on a handle that is no console.
        (unsafe { GetConsoleMode(GetStdHandle(which), &mut mode) } != 0).then_some(mode)
    }

    /// Sets the console mode of a standard handle; whether it worked.
    pub fn set_mode(which: u32, mode: u32) -> bool {
        // SAFETY: `SetConsoleMode` fails on a handle that is no console.
        unsafe { SetConsoleMode(GetStdHandle(which), mode) != 0 }
    }
}

/// After a failed start: when the console window belongs to this process alone (started with a
/// double click), waits for Enter so the window does not close before the message is read.
#[cfg(windows)]
pub fn hold_window() {
    use kernel32::GetConsoleProcessList;
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
