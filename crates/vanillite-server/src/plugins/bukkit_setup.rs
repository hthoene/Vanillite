//! One-step setup of Paper and Spigot plugins (`plugins.bukkit.auto-setup`): when `plugins/`
//! holds Bukkit plugin jars, the Bukkit layer (vanillite-bukkit, a native plugin that hosts a
//! JVM) and a Java 25 runtime are installed into `plugins/bukkit/` before the plugins load.
//! The layer release is pinned with its SHA-256 per target (`bukkit-layer.txt`); the runtime is
//! Eclipse Temurin from Adoptium, checked against the SHA-256 Adoptium publishes. Without
//! Bukkit jars nothing happens.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Path, PathBuf};

use sha2::{Digest as _, Sha256};
use vanillite_config::BukkitRules;
use vanillite_plugin_host::loader;

/// The pinned layer release (see the file).
const PINS: &str = include_str!("../../bukkit-layer.txt");
/// Where the layer releases are published.
const RELEASES: &str = "https://github.com/hthoene/vanillite-bukkit/releases";
/// Java release the layer needs.
const JAVA: u32 = 25;

type Result<T> = std::result::Result<T, String>;

/// The layer archive for this platform: file name, download URL and SHA-256.
struct Archive {
    version: String,
    file: String,
    url: String,
    sha256: String,
}

/// The pinned archive of this target, if the pinned release has one.
fn pinned(target: &str) -> Option<Archive> {
    let lines = PINS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'));
    let mut version = None;
    for line in lines {
        if let Some(v) = line.strip_prefix("version ") {
            version = Some(v.trim().to_owned());
            continue;
        }
        let (Some(version), Some((sha256, file))) =
            (&version, line.split_once(char::is_whitespace))
        else {
            continue;
        };
        let file = file.trim().trim_start_matches('*');
        if file == format!("vanillite-bukkit-{version}-{target}.tar.gz") {
            return Some(Archive {
                url: format!("{RELEASES}/download/v{version}/{file}"),
                file: file.to_owned(),
                sha256: sha256.to_ascii_lowercase(),
                version: version.clone(),
            });
        }
    }
    None
}

/// Installs what the Paper and Spigot plugins in `plugins` need, as far as `rules` allow.
/// Failures are logged; the server starts anyway (the layer then reports what is missing).
pub(crate) fn prepare(plugins: &Path, rules: &BukkitRules) {
    let jars = loader::bukkit_jars(plugins);
    if jars.is_empty() {
        return;
    }
    let layer = plugins.join("bukkit");
    let installed = layer.join("plugin.toml").is_file();
    let java = installed && java_found(&layer);
    if installed && java {
        return;
    }
    let names = jars
        .iter()
        .filter_map(|p| p.file_name()?.to_str())
        .collect::<Vec<_>>()
        .join(", ");
    log::info!(
        "Found {} Paper/Spigot plugin(s) ({names}). They run on the Bukkit layer (vanillite-bukkit).",
        jars.len()
    );
    if !installed {
        let Some(archive) = pinned(loader::TARGET) else {
            log::warn!(
                "There is no released Bukkit layer for this Vanillite version on {} yet. Build it \
                 (https://github.com/hthoene/vanillite-bukkit, scripts/build.sh) and copy \
                 build/dist/plugins/bukkit/ to {}.",
                loader::TARGET,
                layer.display()
            );
            return;
        };
        if !rules.auto_setup {
            log::warn!(
                "The Bukkit layer is not installed and plugins.bukkit.auto-setup is off. Download \
                 {} (SHA-256 {}) and put it into {}; the next start unpacks it.",
                archive.url,
                archive.sha256,
                plugins.display()
            );
            return;
        }
        log::info!("Installing the Bukkit layer {} ...", archive.version);
        match install_layer(plugins, &archive) {
            Ok(()) => log::info!(
                "Bukkit layer {} installed in {}.",
                archive.version,
                layer.display()
            ),
            Err(e) => {
                log::error!(
                    "Cannot install the Bukkit layer: {e}. Download {} (SHA-256 {}) and put it \
                     into {}; the next start unpacks it.",
                    archive.url,
                    archive.sha256,
                    plugins.display()
                );
                return;
            }
        }
        if java_found(&layer) {
            return;
        }
    }
    let runtime = layer.join("runtime");
    if !rules.auto_setup {
        log::warn!(
            "No Java {JAVA} runtime for the Bukkit layer and plugins.bukkit.auto-setup is off. \
             Unpack a Java {JAVA} runtime (e.g. Eclipse Temurin from https://adoptium.net) into \
             {}, or set JAVA_HOME or jvm.java-home in {}.",
            runtime.display(),
            layer.join("config.toml").display()
        );
        return;
    }
    log::info!(
        "No Java {JAVA} runtime found: downloading Eclipse Temurin {JAVA} into {} (once) ...",
        runtime.display()
    );
    match install_java(&runtime) {
        Ok(name) => log::info!("Java runtime {name} installed in {}.", runtime.display()),
        Err(e) => log::error!(
            "Cannot download a Java {JAVA} runtime: {e}. Unpack a Java {JAVA} runtime into {}, \
             or set JAVA_HOME.",
            runtime.display()
        ),
    }
}

/// Unpacks the layer archive (`plugins/<file>` if it is there, else downloaded into
/// `plugins/.cache/`) into `plugins/`: it holds `bukkit/`.
fn install_layer(plugins: &Path, archive: &Archive) -> Result<()> {
    let local = plugins.join(&archive.file);
    let path = if local.is_file() {
        local
    } else {
        let cache = plugins.join(".cache");
        fs::create_dir_all(&cache).map_err(|e| format!("{}: {e}", cache.display()))?;
        let path = cache.join(&archive.file);
        if sha256_file(&path).ok().as_deref() != Some(archive.sha256.as_str()) {
            log::info!("Downloading {}", archive.url);
            download(&archive.url, &path)?;
        }
        path
    };
    let actual = sha256_file(&path)?;
    if actual != archive.sha256 {
        return Err(format!(
            "{}: SHA-256 is {actual}, expected {}",
            path.display(),
            archive.sha256
        ));
    }
    let staging = plugins.join(".cache").join("bukkit-layer");
    unpack(&path, &staging)?;
    let unpacked = staging.join("bukkit");
    if !unpacked.join("plugin.toml").is_file() {
        return Err(format!("{} has no bukkit/plugin.toml", archive.file));
    }
    // Files of an earlier install (config, libraries) stay; the layer's own files are replaced.
    let layer = plugins.join("bukkit");
    merge(&unpacked, &layer)?;
    remove_dir(&staging)?;
    Ok(())
}

/// Moves the entries of `from` into `to`, replacing files of the same name.
fn merge(from: &Path, to: &Path) -> Result<()> {
    fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for entry in fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let target = to.join(entry.file_name());
        if entry.path().is_dir() && target.is_dir() {
            merge(&entry.path(), &target)?;
        } else {
            if target.is_dir() {
                remove_dir(&target)?;
            }
            fs::rename(entry.path(), &target).map_err(|e| format!("{}: {e}", target.display()))?;
        }
    }
    Ok(())
}

/// Downloads Eclipse Temurin (JRE) for this machine into `runtime/<name>/`; returns the name.
fn install_java(runtime: &Path) -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "mac",
        os => os,
    };
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x64",
        arch => arch,
    };
    let api = format!(
        "https://api.adoptium.net/v3/assets/latest/{JAVA}/hotspot?architecture={arch}&image_type=jre&os={os}&vendor=eclipse"
    );
    let mut body = Vec::new();
    agent()
        .get(&api)
        .call()
        .map_err(|e| format!("{api}: {e}"))?
        .into_body()
        .into_reader()
        .read_to_end(&mut body)
        .map_err(|e| format!("{api}: {e}"))?;
    let assets: serde_json::Value =
        serde_json::from_slice(&body).map_err(|e| format!("{api}: {e}"))?;
    let package = &assets[0]["binary"]["package"];
    let field = |name: &str| {
        package[name]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| format!("{api}: no {name} for {os}/{arch}"))
    };
    let (link, sha256, name) = (field("link")?, field("checksum")?, field("name")?);
    if !name.ends_with(".tar.gz") {
        return Err(format!("{name}: only .tar.gz runtimes are unpacked"));
    }
    let staging = runtime.with_extension("part");
    let archive = runtime.with_file_name(&name);
    log::info!("Downloading {link}");
    download(&link, &archive)?;
    let actual = sha256_file(&archive)?;
    if actual != sha256.to_ascii_lowercase() {
        let _ = fs::remove_file(&archive);
        return Err(format!("{name}: SHA-256 is {actual}, expected {sha256}"));
    }
    let unpacked = unpack(&archive, &staging);
    fs::remove_file(&archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    unpacked?;
    remove_dir(runtime)?;
    fs::rename(&staging, runtime).map_err(|e| format!("{}: {e}", runtime.display()))?;
    Ok(name.trim_end_matches(".tar.gz").to_owned())
}

/// Whether the layer in `layer` finds a Java runtime the way it searches: `jvm.java-home` of
/// its `config.toml` (trusted as set), `runtime/`, `$JAVA_HOME`, then `java` on the `PATH`.
fn java_found(layer: &Path) -> bool {
    let configured = fs::read_to_string(layer.join("config.toml"))
        .ok()
        .and_then(|text| text.parse::<vanillite_config::toml::Table>().ok())
        .and_then(|table| {
            table
                .get("jvm")?
                .get("java-home")?
                .as_str()
                .map(str::to_owned)
        })
        .is_some_and(|home| !home.is_empty());
    if configured || runtime_home(&layer.join("runtime")).is_some() {
        return true;
    }
    if std::env::var_os("JAVA_HOME").is_some_and(|home| usable_java(Path::new(&home))) {
        return true;
    }
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("java"))
            .filter(|java| java.is_file())
            .filter_map(|java| fs::canonicalize(java).ok())
            .any(|java| {
                java.parent()
                    .and_then(Path::parent)
                    .is_some_and(usable_java)
            })
    })
}

/// The Java home in `runtime` (itself or one directory below, as an archive unpacks).
fn runtime_home(runtime: &Path) -> Option<PathBuf> {
    if usable_java(runtime) {
        return Some(runtime.to_owned());
    }
    fs::read_dir(runtime)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .find(|home| usable_java(home))
}

/// Whether `home` is a Java home with `libjvm` of at least Java [`JAVA`] (by its `release`
/// file; a home without one is taken as it is).
fn usable_java(home: &Path) -> bool {
    let libjvm = home
        .join("lib")
        .join("server")
        .join(loader::library_file("jvm"));
    if !libjvm.is_file() {
        return false;
    }
    let Ok(release) = fs::read_to_string(home.join("release")) else {
        return true;
    };
    release
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))
        .map(|v| v.trim_matches('"'))
        .and_then(|v| {
            let mut parts = v.split(['.', '_', '-', '+']);
            let first: u32 = parts.next()?.parse().ok()?;
            if first == 1 {
                parts.next()?.parse().ok()
            } else {
                Some(first)
            }
        })
        .is_none_or(|major| major >= JAVA)
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(std::time::Duration::from_secs(30)))
        .build()
        .into()
}

/// Downloads `url` to `to` (through `<to>.part`).
fn download(url: &str, to: &Path) -> Result<()> {
    let part = to.with_extension("part");
    let result = (|| {
        let mut file = fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
        let mut reader = agent()
            .get(url)
            .call()
            .map_err(|e| format!("{url}: {e}"))?
            .into_body()
            .into_with_config()
            .limit(1 << 30)
            .reader();
        std::io::copy(&mut reader, &mut file).map_err(|e| format!("{url}: {e}"))?;
        file.flush()
            .map_err(|e| format!("{}: {e}", part.display()))?;
        fs::rename(&part, to).map_err(|e| format!("{}: {e}", to.display()))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// Unpacks a `.tar.gz` into `dir` (emptied first); entries cannot leave `dir`.
fn unpack(archive: &Path, dir: &Path) -> Result<()> {
    remove_dir(dir)?;
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let file = fs::File::open(archive).map_err(|e| format!("{}: {e}", archive.display()))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(std::io::BufReader::new(file)));
    tar.set_preserve_permissions(true);
    tar.unpack(dir)
        .map_err(|e| format!("{}: {e}", archive.display()))
}

fn remove_dir(dir: &Path) -> Result<()> {
    match fs::remove_dir_all(dir) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            Err(format!("{}: {e}", dir.display()))
        }
        _ => Ok(()),
    }
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
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

#[cfg(test)]
mod tests {
    #[test]
    fn pins_parse() {
        assert!(super::pinned("no-such-target").is_none());
        let target = vanillite_plugin_host::loader::TARGET;
        if let Some(archive) = super::pinned(target) {
            assert_eq!(archive.sha256.len(), 64);
            assert!(archive.url.ends_with(&archive.file));
        }
    }
}
