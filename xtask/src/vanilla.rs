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
    let manifest = manifest()?;
    let x = extract(root, &manifest, version, local_jar, Extractor::Full)?;
    let out = root.join("data").join("vanilla");
    let before = normalize::Snapshot::load(&out);
    normalize::run(&x.generated.join("reports"), &x.extracted, &x.info, &out)?;
    codegen::run(root, false)?;
    let after = normalize::Snapshot::load(&out);

    let report = normalize::change_report(&before, &after);
    let report_path = x.work.join("change-report.md");
    crate::write(&report_path, &report)?;
    println!("{report}");
    println!("change report: {}", report_path.display());
    Ok(())
}

/// Mojang's version manifest.
pub(crate) fn manifest() -> Result<Value> {
    http_json(MANIFEST)
}

/// A version's server jar run through Mojang's data generator and our extractor, in
/// `target/vanilla/<id>/`.
pub(crate) struct Extracted {
    /// `release`, `snapshot`, ... from the manifest.
    pub kind: String,
    pub release_time: String,
    pub work: PathBuf,
    /// The unbundled server jar (classes and the built-in data pack).
    pub inner_jar: PathBuf,
    /// Datagen output (`reports/`).
    pub generated: PathBuf,
    /// Extractor output.
    pub extracted: PathBuf,
    /// Whether Mojang published mappings (none since the jars are unobfuscated).
    pub mappings: bool,
    pub info: VersionInfo,
}

/// Which extractor runs against a jar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Extractor {
    /// Everything `data/vanilla/` needs (`Main`, needs the current API).
    Full,
    /// What `data/multiversion/` needs (`MultiversionMain`, runs against every version from
    /// 1.20.5 on; obfuscated jars are remapped with Mojang's mappings first).
    Multiversion,
}

/// Downloads (or takes) the server jar of `version` (default: the latest release), verifies its
/// SHA-1, fetches the mappings if there are any, and runs datagen and the extractor.
pub(crate) fn extract(
    root: &Path,
    manifest: &Value,
    version: Option<&str>,
    local_jar: Option<&Path>,
    extractor: Extractor,
) -> Result<Extracted> {
    let id = match version {
        Some(v) => v.to_owned(),
        None => str_at(manifest, &["latest", "release"])?.to_owned(),
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
    let mappings = match meta["downloads"]["server_mappings"]["url"].as_str() {
        Some(url) => {
            let file = work.join("server_mappings.txt");
            if !file.exists() {
                println!("downloading {url}");
                download(url, &file)?;
            }
            true
        }
        None => false,
    };

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
    // Obfuscated jars (before 26.1) get Mojang's names, which the extractor is written against.
    let named = if mappings && extractor == Extractor::Multiversion {
        let named = work.join(format!("server-{id}-named.jar"));
        remap(root, &inner, &work.join("server_mappings.txt"), &named)?;
        named
    } else {
        inner.clone()
    };
    let mut classpath = vec![named];
    collect_jars(&work.join("libraries"), &mut classpath)?;
    classpath.sort();
    let sep = if cfg!(windows) { ";" } else { ":" };
    let cp = classpath
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(sep);

    // Our extractor, compiled against exactly this jar. The golden harness is left out: it uses
    // much more of the game's API and must not stop the data of a new version.
    let classes = work.join("extractor-classes");
    remove_dir(&classes)?;
    let mut sources = Vec::new();
    collect_files(&root.join("tools/extractor/src"), "java", &mut sources)?;
    sources.retain(|p| !p.components().any(|c| c.as_os_str() == "golden"));
    let main = match extractor {
        Extractor::Full => "dev.vanillite.extractor.Main",
        Extractor::Multiversion => {
            sources.retain(|p| p.ends_with("MultiversionMain.java") || p.ends_with("Packets.java"));
            "dev.vanillite.extractor.MultiversionMain"
        }
    };
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
        .arg(main)
        .arg(&extracted))?;

    let version_json: Value = serde_json::from_slice(&zip_entry(&inner, "version.json")?)
        .map_err(|e| format!("version.json: {e}"))?;
    let info = VersionInfo::new(&version_json, jar_url, jar_sha1)?;
    Ok(Extracted {
        kind: entry["type"].as_str().unwrap_or_default().to_owned(),
        release_time: entry["releaseTime"].as_str().unwrap_or_default().to_owned(),
        work,
        inner_jar: inner,
        generated,
        extracted,
        mappings,
        info,
    })
}

/// ASM (BSD-3-Clause) for the remapper, from Maven Central: artifact, SHA-1.
const ASM: &[(&str, &str)] = &[
    ("asm", "2ceea6ab43bcae1979b2a6d85fc0ca429877e5ab"),
    ("asm-commons", "ab35de4c537184a09339069f1a3b3aacf2289149"),
    ("asm-tree", "b6b1b3366296163b4b1f540731aad0a2baa484d8"),
];
const ASM_VERSION: &str = "9.9.1";

/// Applies Mojang's mappings to an obfuscated jar (`tools/remapper/`). The result is kept.
fn remap(root: &Path, jar: &Path, mappings: &Path, out: &Path) -> Result<()> {
    if out.exists() {
        return Ok(());
    }
    let tools = root.join("target").join("tools").join("remapper");
    std::fs::create_dir_all(&tools).map_err(|e| format!("{}: {e}", tools.display()))?;
    let mut classpath = Vec::new();
    for (artifact, sha1) in ASM {
        let file = tools.join(format!("{artifact}-{ASM_VERSION}.jar"));
        if !file.exists() || sha1_file(&file)? != *sha1 {
            download(
                &format!(
                    "https://repo1.maven.org/maven2/org/ow2/asm/{artifact}/{ASM_VERSION}/{artifact}-{ASM_VERSION}.jar"
                ),
                &file,
            )?;
            if sha1_file(&file)? != *sha1 {
                return Err(format!("{}: SHA-1 mismatch", file.display()));
            }
        }
        classpath.push(file.display().to_string());
    }
    let sep = if cfg!(windows) { ";" } else { ":" };
    let classes = tools.join("classes");
    remove_dir(&classes)?;
    run(Command::new(java_tool("javac"))
        .args(["-encoding", "UTF-8", "-nowarn", "-d"])
        .arg(&classes)
        .arg("-cp")
        .arg(classpath.join(sep))
        .arg(root.join("tools/remapper/Remap.java")))?;
    classpath.push(classes.display().to_string());
    let tmp = out.with_extension("part");
    run(Command::new(java_tool("java"))
        .arg("-cp")
        .arg(classpath.join(sep))
        .arg("dev.vanillite.remapper.Remap")
        .arg(jar)
        .arg(mappings)
        .arg(&tmp))?;
    std::fs::rename(&tmp, out).map_err(|e| format!("{}: {e}", out.display()))
}

/// `VANILLITE_JAVA_HOME`, then `JAVA_HOME`, then the tool on `PATH`.
pub(crate) fn java_tool(name: &str) -> PathBuf {
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

pub(crate) fn run(cmd: &mut Command) -> Result<()> {
    let status = cmd
        .status()
        .map_err(|e| format!("{:?}: {e}", cmd.get_program()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{:?} failed: {status}", cmd.get_program()))
    }
}

pub(crate) fn str_at<'a>(v: &'a Value, path: &[&str]) -> Result<&'a str> {
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

pub(crate) fn download(url: &str, to: &Path) -> Result<()> {
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

pub(crate) fn sha1_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha1::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = std::io::Read::read(&mut file, &mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
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

pub(crate) fn remove_dir(dir: &Path) -> Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{}: {e}", dir.display())),
    }
}

pub(crate) fn collect_jars(dir: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
    collect_files(dir, "jar", out)
}

pub(crate) fn collect_files(dir: &Path, ext: &str, out: &mut Vec<PathBuf>) -> Result<()> {
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
