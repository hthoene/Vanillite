//! The Minecraft EULA check (Vanilla's `Eula`): the server only starts once `eula.txt` says
//! `eula=true`, or when `VANILLITE_EULA=true` is set (Vanilla's `-Dcom.mojang.eula.agree=true`).

use std::fs;
use std::io;
use std::path::Path;

/// The file in the server folder.
pub const EULA_FILE: &str = "eula.txt";
/// The environment override.
pub const EULA_ENV: &str = "VANILLITE_EULA";

/// Whether the EULA is accepted. Creates `eula.txt` with `eula=false` if it is missing.
pub fn check(dir: &Path) -> io::Result<bool> {
    if std::env::var(EULA_ENV).is_ok_and(|v| v.eq_ignore_ascii_case("true")) {
        return Ok(true);
    }
    let path = dir.join(EULA_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => Ok(text.lines().any(|line| {
            line.split_once('=')
                .is_some_and(|(k, v)| k.trim() == "eula" && v.trim().eq_ignore_ascii_case("true"))
        })),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            fs::write(
                &path,
                "#By changing the setting below to TRUE you are indicating your agreement to our \
                 EULA (https://aka.ms/MinecraftEULA).\neula=false\n",
            )?;
            Ok(false)
        }
        Err(e) => Err(e),
    }
}
