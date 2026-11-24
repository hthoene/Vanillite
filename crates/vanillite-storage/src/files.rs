//! Crash-safe file writes and NBT files.
//!
//! A file is written to `<name>.tmp`, synced, and renamed over the old one, so a crash leaves
//! either the old or the new file complete. With `keep_old` the previous version stays as
//! `<name>_old` (Vanilla's `level.dat_old` and `<uuid>.dat_old`), and reads fall back to it when
//! the file itself is missing or damaged.

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use vanillite_nbt::{Budget, Compound};

use crate::compression::Compression;

/// `<path>` with `suffix` appended to its file name.
pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(suffix);
    path.with_file_name(name)
}

/// Writes `bytes` to `path` atomically. With `keep_old` the previous file becomes
/// `<path>_old`; it is linked before the new file replaces it, so `path` itself is never
/// missing.
pub fn write_atomic(path: &Path, bytes: &[u8], keep_old: bool) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let tmp = with_suffix(path, ".tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    if keep_old && path.exists() {
        let old = with_suffix(path, "_old");
        match fs::remove_file(&old) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        if fs::hard_link(path, &old).is_err() {
            fs::copy(path, &old)?;
        }
    }
    fs::rename(&tmp, path)
}

/// Writes `root` as a gzip-compressed named NBT file (Vanilla's `NbtIo.writeCompressed`).
pub fn write_gzip_nbt(path: &Path, root: &Compound, keep_old: bool) -> io::Result<()> {
    let mut raw = Vec::with_capacity(4096);
    vanillite_nbt::write::write_named("", root, &mut raw).map_err(io::Error::other)?;
    write_atomic(path, &Compression::Gzip.compress(&raw)?, keep_old)
}

/// Reads a gzip-compressed named NBT file; `None` if it does not exist.
pub fn read_gzip_nbt(path: &Path) -> io::Result<Option<Compound>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let raw = Compression::Gzip.decompress(&bytes)?;
    vanillite_nbt::read::read_named(&mut raw.as_slice(), &mut Budget::file())
        .map(Some)
        .map_err(|e| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{}: {e:?}", path.display()),
            )
        })
}

/// Reads `path` like [`read_gzip_nbt`], falling back to `<path>_old` when `path` is missing or
/// damaged (Vanilla does the same for `level.dat` and player files). An error only when neither
/// can be read and `path` exists.
pub fn read_gzip_nbt_or_old(path: &Path) -> io::Result<Option<Compound>> {
    let err = match read_gzip_nbt(path) {
        Ok(Some(root)) => return Ok(Some(root)),
        Ok(None) => None,
        Err(e) => Some(e),
    };
    let old = with_suffix(path, "_old");
    match read_gzip_nbt(&old) {
        Ok(Some(root)) => {
            log::warn!(
                "{} is {}, using {}",
                path.display(),
                if err.is_some() { "damaged" } else { "missing" },
                old.display()
            );
            Ok(Some(root))
        }
        _ => err.map_or(Ok(None), Err),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_write_keeps_old_and_falls_back() {
        let dir = std::env::temp_dir().join(format!("vanillite-files-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("level.dat");
        let one = Compound::new().with("n", 1);
        let two = Compound::new().with("n", 2);
        write_gzip_nbt(&path, &one, true).unwrap();
        assert!(!with_suffix(&path, "_old").exists());
        write_gzip_nbt(&path, &two, true).unwrap();
        assert_eq!(
            read_gzip_nbt(&with_suffix(&path, "_old")).unwrap(),
            Some(one)
        );
        assert_eq!(read_gzip_nbt_or_old(&path).unwrap(), Some(two.clone()));
        assert!(!with_suffix(&path, ".tmp").exists());
        // A damaged file: the previous version is read.
        fs::write(&path, b"garbage").unwrap();
        assert_eq!(
            read_gzip_nbt_or_old(&path).unwrap(),
            Some(Compound::new().with("n", 1))
        );
        fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read_gzip_nbt_or_old(&path).unwrap(), None);
    }
}
