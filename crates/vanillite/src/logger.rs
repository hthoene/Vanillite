//! A small logger in Vanilla's console format: `[HH:MM:SS] [thread/LEVEL]: message`.
//!
//! Times are UTC: the standard library has no time zone database and the logger is not worth
//! a dependency.
//!
//! On a console that shows colours, WARN lines are yellow, ERROR lines red and `§` codes in
//! messages become terminal colours; everywhere else the codes are stripped and lines stay plain
//! text. `VANILLITE_COLOR=always|never` overrides the detection, `NO_COLOR` turns colours off.

use std::fmt::{self, Write as _};
use std::io::{IsTerminal as _, Write as _};
use std::sync::{Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use log::{Level, LevelFilter, Log, Metadata, Record};

/// Where log lines go while the console line editor runs: it prints them above the input line.
type Printer = Box<dyn rustyline::ExternalPrinter + Send>;

static PRINTER: Mutex<Option<Printer>> = Mutex::new(None);

struct ConsoleLogger {
    level: LevelFilter,
    color: bool,
}

impl Log for ConsoleLogger {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        // The line editor's own records would print through itself while it waits for them.
        metadata.level() <= self.level && !metadata.target().starts_with("rustyline")
    }

    fn log(&self, record: &Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let mut printer = PRINTER.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(printer) = printer.as_mut() {
            let mut line = String::with_capacity(128);
            let _ = self.write_line(&mut line, record);
            let _ = printer.print(line);
        } else {
            let mut out = IoWriter(std::io::stderr().lock());
            let _ = self.write_line(&mut out, record);
        }
    }

    fn flush(&self) {
        let _ = std::io::stderr().flush();
    }
}

impl ConsoleLogger {
    fn write_line(&self, out: &mut impl fmt::Write, record: &Record<'_>) -> fmt::Result {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let (h, m, s) = ((secs / 3600) % 24, (secs / 60) % 60, secs % 60);
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("unnamed");
        let (level, base) = match record.level() {
            Level::Error => ("ERROR", "\x1b[31m"),
            Level::Warn => ("WARN", "\x1b[33m"),
            Level::Info => ("INFO", ""),
            Level::Debug => ("DEBUG", ""),
            Level::Trace => ("TRACE", ""),
        };
        let base = if self.color { base } else { "" };
        out.write_str(base)?;
        write!(out, "[{h:02}:{m:02}:{s:02}] [{name}/{level}]: ")?;
        let mut codes = Codes {
            out: &mut *out,
            color: self.color,
            base,
            state: State::Text,
            styled: !base.is_empty(),
        };
        write!(codes, "{}", record.args())?;
        if codes.styled {
            out.write_str("\x1b[0m")?;
        }
        out.write_str("\n")
    }
}

/// `fmt::Write` over an `io::Write`.
struct IoWriter<W>(W);

impl<W: std::io::Write> fmt::Write for IoWriter<W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.0.write_all(s.as_bytes()).map_err(|_| fmt::Error)
    }
}

/// Where [`Codes`] is in a `§` code.
#[derive(Clone, Copy)]
enum State {
    Text,
    /// After `§`.
    Section,
    /// In `§x§r§r§g§g§b§b`: the digits so far, and whether a `§` came last.
    Hex {
        digits: u8,
        rgb: u32,
        section: bool,
    },
}

/// Writes a message with `§` codes as terminal colours (or without them), without buffering it.
struct Codes<'a, W> {
    out: &'a mut W,
    color: bool,
    /// The line's own colour, back after `§r`.
    base: &'static str,
    state: State,
    /// Whether a colour is active and the line has to reset it at its end.
    styled: bool,
}

impl<W: fmt::Write> Codes<'_, W> {
    fn ansi(&mut self, code: &str) -> fmt::Result {
        if self.color {
            self.styled = true;
            self.out.write_str(code)?;
        }
        Ok(())
    }

    fn code(&mut self, c: char) -> fmt::Result {
        match self.state {
            State::Text => self.out.write_char(c),
            State::Section => {
                self.state = State::Text;
                // Colours reset the formats, as in the game.
                let ansi = match c.to_ascii_lowercase() {
                    'x' => {
                        self.state = State::Hex {
                            digits: 0,
                            rgb: 0,
                            section: false,
                        };
                        return Ok(());
                    }
                    '0' => "\x1b[0;30m",
                    '1' => "\x1b[0;34m",
                    '2' => "\x1b[0;32m",
                    '3' => "\x1b[0;36m",
                    '4' => "\x1b[0;31m",
                    '5' => "\x1b[0;35m",
                    '6' => "\x1b[0;33m",
                    '7' => "\x1b[0;37m",
                    '8' => "\x1b[0;90m",
                    '9' => "\x1b[0;94m",
                    'a' => "\x1b[0;92m",
                    'b' => "\x1b[0;96m",
                    'c' => "\x1b[0;91m",
                    'd' => "\x1b[0;95m",
                    'e' => "\x1b[0;93m",
                    'f' => "\x1b[0;97m",
                    'k' => "",
                    'l' => "\x1b[1m",
                    'm' => "\x1b[9m",
                    'n' => "\x1b[4m",
                    'o' => "\x1b[3m",
                    'r' => {
                        self.ansi("\x1b[0m")?;
                        let base = self.base;
                        return self.ansi(base);
                    }
                    _ => {
                        self.out.write_char('§')?;
                        return self.out.write_char(c);
                    }
                };
                self.ansi(ansi)
            }
            State::Hex {
                digits,
                rgb,
                section: false,
            } => {
                if c == '§' {
                    self.state = State::Hex {
                        digits,
                        rgb,
                        section: true,
                    };
                    Ok(())
                } else {
                    self.state = State::Text;
                    self.out.write_char(c)
                }
            }
            State::Hex {
                digits,
                rgb,
                section: true,
            } => {
                let Some(digit) = c.to_digit(16) else {
                    self.state = State::Text;
                    return self.out.write_char(c);
                };
                let rgb = rgb << 4 | digit;
                let digits = digits.saturating_add(1);
                if digits < 6 {
                    self.state = State::Hex {
                        digits,
                        rgb,
                        section: false,
                    };
                    return Ok(());
                }
                self.state = State::Text;
                if self.color {
                    self.styled = true;
                    write!(
                        self.out,
                        "\x1b[0;38;2;{};{};{}m",
                        rgb >> 16,
                        (rgb >> 8) & 0xff,
                        rgb & 0xff
                    )?;
                }
                Ok(())
            }
        }
    }
}

impl<W: fmt::Write> fmt::Write for Codes<'_, W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        let mut rest = s;
        loop {
            if let State::Text = self.state {
                let Some((before, after)) = rest.split_once('§') else {
                    return self.out.write_str(rest);
                };
                self.out.write_str(before)?;
                rest = after;
                self.state = State::Section;
            }
            let mut chars = rest.chars();
            let Some(c) = chars.next() else {
                return Ok(());
            };
            rest = chars.as_str();
            self.code(c)?;
        }
    }
}

/// Whether log lines get colours.
fn color() -> bool {
    match std::env::var("VANILLITE_COLOR").as_deref() {
        Ok("always") => {
            crate::terminal::enable_vt();
            return true;
        }
        Ok("never") => return false,
        _ => {}
    }
    if std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        return false;
    }
    // Pterodactyl's panel console shows colours.
    let console = std::io::stderr().is_terminal() || std::env::var_os("P_SERVER_UUID").is_some();
    console && crate::terminal::enable_vt()
}

/// Installs the logger. The level comes from `VANILLITE_LOG` (default `info`).
pub fn init() {
    let level = std::env::var("VANILLITE_LOG")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(LevelFilter::Info);
    let logger = Box::leak(Box::new(ConsoleLogger {
        level,
        color: color(),
    }));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(level);
    }
}

/// Sends log lines through the console line editor from now on.
pub fn set_printer(printer: Printer) {
    *PRINTER.lock().unwrap_or_else(PoisonError::into_inner) = Some(printer);
}

/// Log lines go straight to stderr again; returns the line editor's printer.
pub fn take_printer() -> Option<Printer> {
    PRINTER
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}
