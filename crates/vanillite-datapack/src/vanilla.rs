//! The Vanilla data pack, taken from Mojang's server jar on first start.
//!
//! Mojang content is never shipped with Vanillite. The server downloads `server.jar` from the
//! URL pinned in `vanillite_data::version` (the user has accepted the EULA), checks its SHA-1,
//! and extracts `data/**` of the inner server jar into `<cache>/vanilla/<sha1>/`. Offline
//! installs pass a local copy of the same jar instead.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};
use vanillite_data::version::{SERVER_JAR_SHA1, SERVER_JAR_URL, VERSION_ID};

use crate::{Error, io_err};

/// Marks a completely extracted pack.
const COMPLETE: &str = ".complete";

/// Returns the directory of the extracted Vanilla data pack (it contains `data/minecraft/`),
/// extracting it first if needed.
///
/// `local_jar` is used instead of a download when given; it must be the pinned jar too.
pub fn ensure_vanilla_pack(cache_dir: &Path, local_jar: Option<&Path>) -> Result<PathBuf, Error> {
    let root = cache_dir.join("vanilla");
    let pack = root.join(SERVER_JAR_SHA1);
    if pack.join(COMPLETE).exists() {
        return Ok(pack);
    }
    std::fs::create_dir_all(&root).map_err(io_err(&root))?;

    let (jar, downloaded) = match local_jar {
        Some(p) => (p.to_path_buf(), false),
        None => {
            let p = root.join(format!("server-{VERSION_ID}.jar"));
            download(SERVER_JAR_URL, &p)?;
            (p, true)
        }
    };
    let actual = sha1_hex(&jar)?;
    if actual != SERVER_JAR_SHA1 {
        if downloaded {
            let _ = std::fs::remove_file(&jar);
        }
        return Err(Error::Checksum {
            path: jar,
            expected: SERVER_JAR_SHA1,
            actual,
        });
    }

    let tmp = root.join(format!("{SERVER_JAR_SHA1}.tmp"));
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).map_err(io_err(&tmp))?;
    }
    std::fs::create_dir_all(&tmp).map_err(io_err(&tmp))?;
    let inner = tmp.join("server.jar");
    copy_entry(
        &jar,
        &format!("META-INF/versions/{VERSION_ID}/server-{VERSION_ID}.jar"),
        &inner,
    )?;
    extract_data(&inner, &tmp)?;
    std::fs::remove_file(&inner).map_err(io_err(&inner))?;
    File::create(tmp.join(COMPLETE)).map_err(io_err(&tmp))?;
    if pack.exists() {
        std::fs::remove_dir_all(&pack).map_err(io_err(&pack))?;
    }
    std::fs::rename(&tmp, &pack).map_err(io_err(&pack))?;
    if downloaded {
        let _ = std::fs::remove_file(&jar);
    }
    Ok(pack)
}

fn open_zip(jar: &Path) -> Result<zip::ZipArchive<File>, Error> {
    let file = File::open(jar).map_err(io_err(jar))?;
    zip::ZipArchive::new(file).map_err(|e| Error::Jar(jar.to_path_buf(), e.to_string()))
}

/// Copies one entry of a zip file to `to`.
fn copy_entry(jar: &Path, name: &str, to: &Path) -> Result<(), Error> {
    let mut zip = open_zip(jar)?;
    let mut entry = zip
        .by_name(name)
        .map_err(|e| Error::Jar(jar.to_path_buf(), format!("{name}: {e}")))?;
    let mut out = BufWriter::new(File::create(to).map_err(io_err(to))?);
    std::io::copy(&mut entry, &mut out).map_err(io_err(to))?;
    out.flush().map_err(io_err(to))
}

/// Extracts every file under `data/` of the inner server jar into `dir`.
fn extract_data(jar: &Path, dir: &Path) -> Result<(), Error> {
    let mut zip = open_zip(jar)?;
    for i in 0..zip.len() {
        let mut entry = zip
            .by_index(i)
            .map_err(|e| Error::Jar(jar.to_path_buf(), e.to_string()))?;
        let name = entry
            .name()
            .map_err(|e| Error::Jar(jar.to_path_buf(), e.to_string()))?;
        if entry.is_dir() || !name.starts_with("data/") {
            continue;
        }
        // `enclosed_name` rejects absolute paths and `..` components.
        let Some(rel) = entry.enclosed_name() else {
            continue;
        };
        let to = dir.join(rel);
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent).map_err(io_err(parent))?;
        }
        let mut out = BufWriter::new(File::create(&to).map_err(io_err(&to))?);
        std::io::copy(&mut entry, &mut out).map_err(io_err(&to))?;
        out.flush().map_err(io_err(&to))?;
    }
    Ok(())
}

fn sha1_hex(path: &Path) -> Result<String, Error> {
    let mut file = File::open(path).map_err(io_err(path))?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf).map_err(io_err(path))?;
        let Some(chunk) = buf.get(..n).filter(|c| !c.is_empty()) else {
            break;
        };
        hasher.update(chunk);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(feature = "fetch")]
fn download(url: &str, to: &Path) -> Result<(), Error> {
    let part = to.with_extension("part");
    let mut reader = ureq::get(url)
        .call()
        .map_err(|e| Error::Download(format!("{url}: {e}")))?
        .into_body()
        .into_reader();
    let mut out = BufWriter::new(File::create(&part).map_err(io_err(&part))?);
    std::io::copy(&mut reader, &mut out).map_err(|e| Error::Download(format!("{url}: {e}")))?;
    out.flush().map_err(io_err(&part))?;
    drop(out);
    std::fs::rename(&part, to).map_err(io_err(to))
}

#[cfg(not(feature = "fetch"))]
fn download(url: &str, _to: &Path) -> Result<(), Error> {
    Err(Error::Download(format!(
        "built without the `fetch` feature; download {url} and pass it as a local jar"
    )))
}
