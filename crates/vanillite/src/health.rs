//! `vanillite health`: a status ping against the local server, for container health checks.

use std::io::{Read as _, Write as _};
use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use vanillite_config::{PROPERTIES_FILE, env_name, properties};

const TIMEOUT: Duration = Duration::from_secs(5);
/// The largest status response read (Vanilla's string limit, in bytes).
const MAX_RESPONSE: usize = 32767 * 3 + 5;

pub fn run(mut args: impl Iterator<Item = String>, usage: &str) -> ExitCode {
    let mut dir = PathBuf::from(".");
    let mut port = None;
    while let Some(arg) = args.next() {
        match (arg.as_str(), args.next()) {
            ("--dir", Some(path)) => dir = PathBuf::from(path),
            ("--port", Some(value)) => match value.parse::<u16>() {
                Ok(value) => port = Some(value),
                Err(_) => {
                    eprintln!("Invalid port {value:?}");
                    return ExitCode::FAILURE;
                }
            },
            _ => {
                eprintln!("Invalid option {arg:?}\n\n{usage}");
                return ExitCode::FAILURE;
            }
        }
    }
    let port = port.unwrap_or_else(|| configured_port(&dir));
    match ping(port) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("Status ping on port {port} failed: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The port the server listens on, read like the server reads it.
fn configured_port(dir: &std::path::Path) -> u16 {
    let from_env = std::env::var(env_name("server-port")).ok();
    let from_file = || {
        let text = std::fs::read_to_string(dir.join(PROPERTIES_FILE)).ok()?;
        properties::parse(&text).remove("server-port")
    };
    from_env
        .or_else(from_file)
        .and_then(|raw| raw.trim().parse().ok())
        .unwrap_or(25565)
}

/// Handshake with intent status, status request, and a JSON status response back.
fn ping(port: u16) -> std::io::Result<()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, TIMEOUT)?;
    stream.set_read_timeout(Some(TIMEOUT))?;
    stream.set_write_timeout(Some(TIMEOUT))?;

    let mut handshake = vec![0x00];
    write_var_int(&mut handshake, -1); // Any protocol version may ask for the status.
    write_var_int(&mut handshake, 9);
    handshake.extend_from_slice(b"localhost");
    handshake.extend_from_slice(&port.to_be_bytes());
    write_var_int(&mut handshake, 1); // Next state: status.
    let mut out = Vec::new();
    write_var_int(&mut out, handshake.len() as i32);
    out.extend_from_slice(&handshake);
    out.extend_from_slice(&[0x01, 0x00]); // Status request.
    stream.write_all(&out)?;

    let length = read_var_int(&mut stream)?;
    let length = usize::try_from(length)
        .ok()
        .filter(|&l| l > 0 && l <= MAX_RESPONSE)
        .ok_or_else(|| invalid("bad packet length"))?;
    let mut packet = vec![0; length];
    stream.read_exact(&mut packet)?;
    let mut body = packet.as_slice();
    if read_var_int(&mut body)? != 0x00 {
        return Err(invalid("not a status response"));
    }
    read_var_int(&mut body)?;
    if body.first() != Some(&b'{') {
        return Err(invalid("status response is not JSON"));
    }
    Ok(())
}

fn write_var_int(out: &mut Vec<u8>, value: i32) {
    let mut value = value as u32;
    loop {
        if value < 0x80 {
            out.push(value as u8);
            return;
        }
        out.push((value as u8 & 0x7f) | 0x80);
        value >>= 7;
    }
}

fn read_var_int(input: &mut impl std::io::Read) -> std::io::Result<i32> {
    let mut value = 0u32;
    for shift in (0..35).step_by(7) {
        let mut byte = [0];
        input.read_exact(&mut byte)?;
        value |= u32::from(byte[0] & 0x7f) << shift;
        if byte[0] & 0x80 == 0 {
            return Ok(value as i32);
        }
    }
    Err(invalid("VarInt too long"))
}

fn invalid(message: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, message)
}
