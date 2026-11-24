//! `cargo xtask vanilla update`: jar download, datagen, extractor, normalisation, codegen.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;
use sha1::{Digest, Sha1};

use crate::normalize::{self, VersionInfo};
use crate::{Result, codegen};

const MANIFEST: &str = "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

pub fn update(root: &Path, version: Option<&str>, local_jar: Option<&Path>) -> Result<()> {
    let manifest = http_json(MANIFEST)?;
    let id = match version {
        Some(v) => v.to_owned(),
        None => str_at(&manifest, &["latest", "release"])?.to_owned(),
    };
    let entry = manifest["versions"]
        .as_array()
        .and_then(|vs| vs.iter().find(|v| v["id"] == id.as_str()))
        .ok_or_else(|| format!("version {id} is not in the version manifest"))?;
    let meta = http_json(str_at(entry, &["url"])?)?;
    let jar_url = str_at(&meta, &["downloads", "server", "url"])?.to_owned();
    let jar_sha1 = str_at(&meta, &["downloads", "server", "sha1"])?.to_owned();
    let java_major = meta["javaVersion"]["majorVersion"].as_u64().unwrap_or(0);
    println!("vanilla update: {id} (server.jar {jar_sha1}, Java {java_major})");

    let work = root.join("target").join("vanilla").join(&id);
    std::fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let jar = work.join("server.jar");
    if let Some(local) = local_jar {
        std::fs::copy(local, &jar).map_err(|e| format!("{}: {e}", local.display()))?;
    } else if !jar.exists() || sha1_file(&jar)? != jar_sha1 {
        println!("downloading {jar_url}");
        download(&jar_url, &jar)?;
    }
    let actual = sha1_file(&jar)?;
    if actual != jar_sha1 {
        return Err(format!(
            "server.jar SHA-1 mismatch: expected {jar_sha1}, got {actual}"
        ));
    }

    // Mojang's data generator. The bundler also unpacks the inner jar and its libraries.
    let generated = work.join("generated");
    remove_dir(&generated)?;
    run(Command::new(java_tool("java"))
        .current_dir(&work)
        .arg("-DbundlerMainClass=net.minecraft.data.Main")
        .args(["-jar", "server.jar", "--reports", "--output", "generated"]))
    .map_err(|e| format!("{e}\n(datagen needs Java {java_major})"))?;

    let inner = work
        .join("versions")
        .join(&id)
        .join(format!("server-{id}.jar"));
    let mut classpath = vec![inner.clone()];
    collect_jars(&work.join("libraries"), &mut classpath)?;
    classpath.sort();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = classpath
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(sep);

    // Our extractor, compiled against exactly this jar.
    let classes = work.join("extractor-classes");
    remove_dir(&classes)?;
    let mut sources = Vec::new();
    collect_files(&root.join("tools/extractor/src"), "java", &mut sources)?;
    sources.sort();
    run(Command::new(java_tool("javac"))
        .args(["-encoding", "UTF-8", "-nowarn", "-d"])
        .arg(&classes)
        .arg("-cp")
        .arg(&cp)
        .args(&sources))?;
    let extracted = work.join("extracted");
    remove_dir(&extracted)?;
    run(Command::new(java_tool("java"))
        .current_dir(&work)
        .arg("-cp")
        .arg(format!("{}{sep}{cp}", classes.display()))
        .arg("dev.vanillite.extractor.Main")
        .arg(&extracted))?;

    let version_json: Value = serde_json::from_slice(&zip_entry(&inner, "version.json")?)
        .map_err(|e| format!("version.json: {e}"))?;
    let info = VersionInfo::new(&version_json, jar_url, jar_sha1)?;

    let out = root.join("data").join("vanilla");
    let before = normalize::Snapshot::load(&out);
    normalize::run(&generated.join("reports"), &extracted, &info, &out)?;
    codegen::run(root, false)?;
    let after = normalize::Snapshot::load(&out);

    let report = normalize::change_report(&before, &after);
    let report_path = work.join("change-report.md");
    crate::write(&report_path, &report)?;
    println!("{report}");
    println!("change report: {}", report_path.display());
    Ok(())
}

/// `VANILLITE_JAVA_HOME`, then `JAVA_HOME`, then the tool on `PATH`.
fn java_tool(name: &str) -> PathBuf {
    for var in ["VANILLITE_JAVA_HOME", "JAVA_HOME"] {
        if let Some(home) = std::env::var_os(var) {
            let tool = PathBuf::from(home).join("bin").join(name);
            if tool.exists() {
                return tool;
            }
        }
    }
    PathBuf::from(name)
}

fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .status()
        .map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed: {status}", cmd.get_program()))
    }
}

fn str_at<'a>(v: &'a Value, path: &[&str]) -> Result<&'a str> {
    path.iter()
        .fold(v, |v, k| &v[*k])
        .as_str()
        .ok_or_else(|| format!("missing {} in Mojang metadata", path.join(".")))
}

fn http_json(url: &str) -> Result<Value> {
    let mut body = Vec::new();
    ureq::get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .into_body()
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("{url}: {e}"))?;
    serde_json::from_slice(&body).map_err(|e| format!("{url}: {e}"))
}

fn download(url: &str, to: &Path) -> Result<()> {
    let tmp = to.with_extension("part");
    let mut file = std::fs::File::create(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    let mut reader = ureq::get(url)
        .call()
        .map_err(|e| format!("{url}: {e}"))?
        .into_body()
        .into_reader();
    std::io::copy(&mut reader, &mut file).map_err(|e| format!("{url}: {e}"))?;
    file.flush()
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, to).map_err(|e| format!("{}: {e}", to.display()))
}

fn sha1_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha1::new();
    std::io::copy(&mut file, &mut hasher).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

fn zip_entry(jar: &Path, name: &str) -> Result<Vec<u8>> {
    let file = std::fs::File::open(jar).map_err(|e| format!("{}: {e}", jar.display()))?;
    let mut zip = zip::ZipArchive::new(file).map_err(|e| format!("{}: {e}", jar.display()))?;
    let mut entry = zip
        .by_name(name)
        .map_err(|e| format!("{}!{name}: {e}", jar.display()))?;
    let mut buf = Vec::new();
    entry
        .read_to_end(&mut buf)
        .map_err(|e| format!("{}!{name}: {e}", jar.display()))?;
    Ok(buf)
}

fn remove_dir(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

fn collect_jars(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    collect_files(dir, "jar", out)
}

fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = std::fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for entry in entries {
        let path = entry.map_err(|e| format!("{}: {e}", dir.display()))?.path();
        if path.is_dir() {
            collect_files(&path, ext, out)?;
        } else if path.extension().is_some_and(|e| e == ext) {
            out.push(path);
        }
    }
    Ok(())
}
