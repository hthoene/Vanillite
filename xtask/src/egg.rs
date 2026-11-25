//! `cargo xtask egg`: the Pterodactyl egg `deploy/pterodactyl/egg-vanillite.json`, generated
//! from the typed configuration schema of `vanillite-config`.
//!
//! Every `server.properties` key marked for panels and every rule of `vanillite.toml` (except
//! the `debug` helpers) becomes an egg variable named like its `VANILLITE_<KEY>` override. Rule
//! variables default to empty: an empty variable is ignored by the server, so the profile and
//! `vanillite.toml` decide until a value is entered in the panel.

use std::collections::BTreeSet;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use vanillite_config::{
    PropertyKind, RuleInfo, RuleSet, ServerProperties, ServerRules, ValueType, WorldRules, env_name,
};

use crate::Result;

/// The egg, relative to the repository root.
pub const EGG_PATH: &str = "deploy/pterodactyl/egg-vanillite.json";
/// The image on Docker Hub (published by `.github/workflows/docker.yml`).
const IMAGE: &str = "thoenehannes/vanillite:latest";
/// The startup command; Wings replaces `{{SERVER_PORT}}` with the allocation's port.
const STARTUP: &str =
    "env VANILLITE_SERVER_PORT={{SERVER_PORT}} /vanillite run --dir /home/container";
/// The console line that marks a finished start.
const DONE: &str = ")! For help, type ";
/// Rule groups that never belong on a hosted server.
const HIDDEN_GROUPS: &[&str] = &["debug"];
/// Environment variables Pterodactyl sets itself; eggs must not declare them.
const RESERVED: &[&str] = &[
    "SERVER_MEMORY",
    "SERVER_IP",
    "SERVER_PORT",
    "ENV",
    "HOME",
    "USER",
    "STARTUP",
    "SERVER_UUID",
    "UUID",
];
const INSTALL_SCRIPT: &str = "#!/bin/ash
# Vanillite needs no installation: the image holds the server, and the first start downloads
# Mojang's server jar for the Vanilla data into cache/.
mkdir -p /mnt/server
echo \"Nothing to install.\"
";

/// `cargo xtask egg [--check]` and `cargo xtask egg smoke [--image <ref>]`.
pub fn run(root: &Path, args: &[&str]) -> Result<()> {
    match args {
        [] => {
            let egg = render();
            check_format(&egg)?;
            crate::write(&root.join(EGG_PATH), &egg)?;
            println!("wrote {EGG_PATH}");
            Ok(())
        }
        ["--check"] => check(root),
        ["smoke"] => smoke(&default_image()),
        ["smoke", "--image", image] => smoke(image),
        _ => Err(crate::USAGE.to_owned()),
    }
}

/// Fails if the committed egg differs from the generated one or breaks the egg format.
pub fn check(root: &Path) -> Result<()> {
    let egg = render();
    check_format(&egg)?;
    let committed = crate::read(&root.join(EGG_PATH)).unwrap_or_default();
    if committed != egg {
        return Err(format!(
            "{EGG_PATH} is out of date with the configuration schema; run `cargo xtask egg`"
        ));
    }
    Ok(())
}

/// The egg as pretty JSON with a trailing newline.
pub fn render() -> String {
    let mut variables = vec![
        variable(
            "Accept the Minecraft EULA",
            "Set to true to accept the Minecraft EULA (https://aka.ms/MinecraftEULA). The \
             server does not start before. The panel's EULA prompt writes eula.txt instead.",
            "VANILLITE_EULA",
            "false",
            "required|string|in:true,false",
        ),
        variable(
            "Profile",
            "Preset of every vanillite.toml rule: vanilla (Vanilla behaviour), technical \
             (Vanilla plus what technical players need), paper or spigot (their defaults), \
             performance (Vanillite's optimizations that deviate a little from Vanilla). Rules \
             set below or in vanillite.toml override the profile.",
            "VANILLITE_PROFILE",
            "vanilla",
            "required|string|in:vanilla,technical,paper,spigot,performance",
        ),
        variable(
            "Log Level",
            "Console log level: error, warn, info, debug or trace.",
            "VANILLITE_LOG",
            "info",
            "required|string|in:error,warn,info,debug,trace",
        ),
    ];

    let mut defaults = std::collections::BTreeMap::new();
    ServerProperties::default().write_to_map(&mut defaults);
    for info in ServerProperties::INFO.iter().filter(|i| i.panel) {
        let default = defaults.get(info.key).cloned().unwrap_or_default();
        let rules = match (info.kind, info.range) {
            (PropertyKind::Bool, _) => "required|string|in:true,false".to_owned(),
            (PropertyKind::Integer, Some((min, max))) => {
                format!("required|integer|between:{min},{max}")
            }
            (PropertyKind::Integer, None) => "required|integer".to_owned(),
            (PropertyKind::Text, _) => "nullable|string".to_owned(),
            (PropertyKind::Enum(names), _) => format!("required|string|in:{}", names.join(",")),
        };
        let mut description = sentence(info.doc);
        if let Some((min, max)) = info.range {
            description.push_str(&format!(" Range: {min} to {max}."));
        }
        description.push_str(&format!(" server.properties: {}.", info.key));
        if info.kind == PropertyKind::Text {
            description.push_str(" Empty: the value in server.properties.");
        }
        variables.push(variable(
            &title(info.key),
            &description,
            &env_name(info.key),
            &default,
            &rules,
        ));
    }

    rule_variables::<ServerRules>(&mut variables, "");
    rule_variables::<WorldRules>(&mut variables, " Applies to every world.");

    let config = |value: Value| Value::String(serde_json::to_string(&value).unwrap_or_default());
    let egg = json!({
        "_comment": "DO NOT EDIT: generated from the vanillite-config schema by `cargo xtask egg`",
        "meta": { "version": "PTDL_v2", "update_url": null },
        "exported_at": "2026-11-24T00:00:00+00:00",
        "name": "Vanillite",
        "author": "hthoene@users.noreply.github.com",
        "description": "Vanillite: a Minecraft Java Edition server written in Rust that behaves \
                        like Vanilla in game; fast, multithreaded and low on RAM. Every \
                        server.properties and vanillite.toml setting is a variable. \
                        https://github.com/hthoene/Vanillite",
        "features": ["eula"],
        "docker_images": { "Vanillite": IMAGE },
        "file_denylist": [],
        "startup": STARTUP,
        "config": {
            "files": config(json!({})),
            "startup": config(json!({ "done": DONE })),
            "logs": config(json!({})),
            "stop": "stop",
        },
        "scripts": {
            "installation": {
                "script": INSTALL_SCRIPT,
                "container": "ghcr.io/pterodactyl/installers:alpine",
                "entrypoint": "ash",
            }
        },
        "variables": variables,
    });
    let mut out = serde_json::to_string_pretty(&egg).unwrap_or_default();
    out.push('\n');
    out
}

fn rule_variables<R: RuleSet>(variables: &mut Vec<Value>, scope: &str) {
    let vanilla = R::for_profile(vanillite_config::Profile::Vanilla);
    for info in R::RULES {
        if HIDDEN_GROUPS.contains(&info.group) {
            continue;
        }
        let shown = vanilla
            .get(info.key)
            .map(|v| match v {
                toml::Value::String(s) => s,
                other => other.to_string(),
            })
            .unwrap_or_default();
        let mut description = sentence(info.doc);
        description.push_str(scope);
        if let Some((min, max)) = info.range {
            description.push_str(&format!(" Range: {min} to {max}."));
        }
        if let Some(keyword) = info.value.keyword {
            description.push_str(&format!(" Also accepts {keyword}."));
        }
        description.push_str(&format!(
            " Empty: the profile's value (vanilla profile: {shown}). vanillite.toml: {}.",
            info.key
        ));
        let group = info.group.rsplit('.').next().unwrap_or(info.group);
        variables.push(variable(
            &format!("{}: {}", title(group), title(info.name)),
            &description,
            &env_name(info.key),
            "",
            &rule_rules(info),
        ));
    }
}

/// The panel's validation rules of a rule variable.
fn rule_rules(info: &RuleInfo) -> String {
    let number = match info.value.ty {
        ValueType::Bool => return "nullable|string|in:true,false".to_owned(),
        ValueType::Text => return "nullable|string".to_owned(),
        ValueType::Integer => ("integer", "[0-9]*"),
        ValueType::Number => ("numeric", "[0-9]*(\\.[0-9]+)?"),
    };
    if let Some(keyword) = info.value.keyword {
        // The keyword or a number (the range is checked by the server on start). No `|` in the
        // pattern: the panel splits the rules on it.
        return format!(
            "nullable|string|regex:/^(?!{keyword}[0-9])({keyword})?{}$/",
            number.1
        );
    }
    match info.range {
        Some((min, max)) => format!("nullable|{}|between:{min},{max}", number.0),
        None => format!("nullable|{}", number.0),
    }
}

fn variable(name: &str, description: &str, env: &str, default: &str, rules: &str) -> Value {
    json!({
        "name": name,
        "description": description,
        "env_variable": env,
        "default_value": default,
        "user_viewable": true,
        "user_editable": true,
        "rules": rules,
        "field_type": "text",
    })
}

/// Joins the lines of a doc comment into one paragraph.
fn sentence(doc: &str) -> String {
    doc.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// `max-players` → `Max Players`, keeping common abbreviations upper case.
fn title(key: &str) -> String {
    key.split(['-', '.', '_'])
        .map(|word| match word {
            "motd" | "rcon" | "jmx" | "tnt" | "nbt" | "id" | "ips" | "url" => match word {
                "ips" => "IPs".to_owned(),
                other => other.to_ascii_uppercase(),
            },
            "sha1" => "SHA-1".to_owned(),
            _ => {
                let mut chars = word.chars();
                chars
                    .next()
                    .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
                    .unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Checks the egg against the PTDL_v2 format the panel imports, and every default value against
/// its own validation rules (the panel checks them when a server is created).
pub fn check_format(egg: &str) -> Result<()> {
    let egg: Value = serde_json::from_str(egg).map_err(|e| format!("egg: {e}"))?;
    let str_at = |pointer: &str| -> Result<&str> {
        egg.pointer(pointer)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("egg: {pointer} must be a string"))
    };
    if str_at("/meta/version")? != "PTDL_v2" {
        return Err("egg: meta.version must be PTDL_v2".to_owned());
    }
    for pointer in [
        "/name",
        "/author",
        "/description",
        "/startup",
        "/config/stop",
    ] {
        if str_at(pointer)?.is_empty() {
            return Err(format!("egg: {pointer} is empty"));
        }
    }
    if !str_at("/author")?.contains('@') {
        return Err("egg: author must be an e-mail address".to_owned());
    }
    for pointer in [
        "/scripts/installation/script",
        "/scripts/installation/container",
        "/scripts/installation/entrypoint",
    ] {
        str_at(pointer)?;
    }
    for pointer in ["/config/files", "/config/startup", "/config/logs"] {
        let parsed: Value = serde_json::from_str(str_at(pointer)?)
            .map_err(|e| format!("egg: {pointer} is not JSON: {e}"))?;
        if !parsed.is_object() {
            return Err(format!("egg: {pointer} must hold a JSON object"));
        }
    }
    let images = egg
        .pointer("/docker_images")
        .and_then(Value::as_object)
        .filter(|m| !m.is_empty() && m.values().all(Value::is_string))
        .ok_or("egg: docker_images must map names to images")?;
    let _ = images;
    for pointer in ["/features", "/file_denylist"] {
        egg.pointer(pointer)
            .and_then(Value::as_array)
            .filter(|a| a.iter().all(Value::is_string))
            .ok_or_else(|| format!("egg: {pointer} must be a list of strings"))?;
    }

    let variables = egg
        .pointer("/variables")
        .and_then(Value::as_array)
        .ok_or("egg: variables must be a list")?;
    let mut envs = BTreeSet::new();
    for var in variables {
        let field = |name: &str| -> Result<&str> {
            var.get(name)
                .and_then(Value::as_str)
                .ok_or_else(|| format!("egg variable {var}: {name} must be a string"))
        };
        let env = field("env_variable")?;
        let name = field("name")?;
        if name.is_empty() || name.chars().count() > 191 {
            return Err(format!("{env}: name must have 1 to 191 characters"));
        }
        field("description")?;
        if env.is_empty()
            || env.len() > 191
            || !env.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            return Err(format!("{env}: invalid environment variable name"));
        }
        if RESERVED.contains(&env) {
            return Err(format!("{env}: reserved by Pterodactyl"));
        }
        if !envs.insert(env) {
            return Err(format!("{env}: declared twice"));
        }
        for flag in ["user_viewable", "user_editable"] {
            if !var.get(flag).is_some_and(Value::is_boolean) {
                return Err(format!("{env}: {flag} must be true or false"));
            }
        }
        let default = field("default_value")?;
        validate(field("rules")?, default)
            .map_err(|e| format!("{env}: default {default:?}: {e}"))?;
    }
    Ok(())
}

/// Checks a value against Laravel validation rules, as far as the egg uses them.
fn validate(rules: &str, value: &str) -> Result<()> {
    let rules: Vec<&str> = rules.split('|').collect();
    if value.is_empty() {
        return if rules.contains(&"nullable") {
            Ok(())
        } else {
            Err("is required".to_owned())
        };
    }
    let numeric = rules.contains(&"integer") || rules.contains(&"numeric");
    for rule in rules {
        let (name, param) = rule.split_once(':').unwrap_or((rule, ""));
        let ok = match name {
            "required" | "nullable" | "string" => true,
            "integer" => value.parse::<i64>().is_ok(),
            "numeric" => value.parse::<f64>().is_ok(),
            "in" => param.split(',').any(|v| v == value),
            "between" => {
                let (min, max) = param
                    .split_once(',')
                    .and_then(|(a, b)| Some((a.parse::<f64>().ok()?, b.parse::<f64>().ok()?)))
                    .ok_or_else(|| format!("invalid rule {rule}"))?;
                let size = if numeric {
                    value.parse::<f64>().unwrap_or(f64::NAN)
                } else {
                    value.chars().count() as f64
                };
                (min..=max).contains(&size)
            }
            "regex" => regex_matches(param, value)?,
            _ => return Err(format!("unknown rule {rule}")),
        };
        if !ok {
            return Err(format!("fails {rule}"));
        }
    }
    Ok(())
}

/// Matches the only regex shape the egg generates: `/^(?!K[0-9])(K)?<digits>$/`.
fn regex_matches(pattern: &str, value: &str) -> Result<bool> {
    let body = pattern
        .strip_prefix("/^(?!")
        .and_then(|p| p.strip_suffix("$/"))
        .ok_or_else(|| format!("unexpected regex {pattern}"))?;
    let (keyword, rest) = body
        .split_once("[0-9])(")
        .ok_or_else(|| format!("unexpected regex {pattern}"))?;
    let number = rest
        .strip_prefix(keyword)
        .and_then(|r| r.strip_prefix(")?"))
        .ok_or_else(|| format!("unexpected regex {pattern}"))?;
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    let number_ok = |s: &str| match number {
        "[0-9]*" => digits(s),
        "[0-9]*(\\.[0-9]+)?" => match s.split_once('.') {
            Some((a, b)) => digits(a) && !b.is_empty() && digits(b),
            None => digits(s),
        },
        _ => false,
    };
    Ok(value == keyword || number_ok(value))
}

fn default_image() -> String {
    let image = std::env::var("VANILLITE_IMAGE")
        .unwrap_or_else(|_| crate::docker::DEFAULT_IMAGE.to_owned());
    format!("{image}:dev")
}

/// Starts `image` like Pterodactyl's Wings does (startup command and default variables from the
/// egg, the server folder at /home/container, a non-root user, a read-only root file system, a
/// terminal) and checks that the server starts and answers a status ping on the published port.
fn smoke(image: &str) -> Result<()> {
    const NAME: &str = "vanillite-egg-smoke";
    const PORT: &str = "30950";
    let egg: Value = serde_json::from_str(&render()).map_err(|e| e.to_string())?;
    let host = crate::docker::docker_host();
    let docker = || crate::docker::docker(host.as_deref());
    let _ = docker()
        .args(["rm", "-f", NAME])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();

    let mut cmd = docker();
    cmd.args([
        "run",
        "-d",
        "-i",
        "-t",
        "--name",
        NAME,
        "--user",
        "988:988",
        "--read-only",
    ])
    .args(["--tmpfs", "/tmp:rw,exec,mode=1777", "--memory", "4g"])
    .args(["--mount", "type=tmpfs,dst=/home/container,tmpfs-mode=1777"])
    .args(["-p", &format!("{PORT}:{PORT}")])
    .args([
        "-e",
        &format!("STARTUP={}", egg["startup"].as_str().unwrap_or_default()),
    ])
    .args([
        "-e",
        &format!("SERVER_PORT={PORT}"),
        "-e",
        "SERVER_IP=0.0.0.0",
    ])
    .args(["-e", "SERVER_MEMORY=4096", "-e", "TZ=UTC"]);
    for var in egg["variables"].as_array().into_iter().flatten() {
        let env = var["env_variable"].as_str().unwrap_or_default();
        // Accepting the EULA is the one choice the admin has to make.
        let value = if env == "VANILLITE_EULA" {
            "true"
        } else {
            var["default_value"].as_str().unwrap_or_default()
        };
        cmd.args(["-e", &format!("{env}={value}")]);
    }
    cmd.arg(image).stdout(Stdio::null());
    crate::devenv::run(&mut cmd)?;

    let result = wait_and_ping(&docker, image, NAME, PORT);
    if result.is_err() {
        let _ = docker().args(["logs", "--tail", "50", NAME]).status();
    }
    let _ = docker()
        .args(["rm", "-f", NAME])
        .stdout(Stdio::null())
        .status();
    result
}

fn wait_and_ping(docker: &dyn Fn() -> Command, image: &str, name: &str, port: &str) -> Result<()> {
    let start = Instant::now();
    loop {
        let logs = docker()
            .args(["logs", name])
            .output()
            .map_err(|e| format!("docker logs: {e}"))?;
        let text = String::from_utf8_lossy(&logs.stdout);
        if text.contains(DONE) {
            println!("started in {:.0} s", start.elapsed().as_secs_f64());
            break;
        }
        let running = docker()
            .args(["inspect", "-f", "{{.State.Running}}", name])
            .output()
            .map_err(|e| format!("docker inspect: {e}"))?;
        if String::from_utf8_lossy(&running.stdout).trim() != "true" {
            return Err("the container stopped before the server was started".to_owned());
        }
        if start.elapsed() > Duration::from_secs(600) {
            return Err("the server did not start within 10 minutes".to_owned());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    // The ping comes from a second container on the host network, through the published port.
    crate::devenv::run(docker().args([
        "run",
        "--rm",
        "--network",
        "host",
        image,
        "health",
        "--port",
        port,
    ]))?;
    println!("status ping answered on port {port}");
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn committed_egg_is_current() {
        if let Err(e) = super::check(&crate::repo_root()) {
            panic!("{e}");
        }
    }
}
