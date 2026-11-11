//! Extensions: packages (`.sshx`, a zip with `extension.json` and its files) that add to the
//! program, installed from a file or from the online registry (sshremote.dev), updated and
//! removed. They contribute background layers (Lottie animations and pictures) to Appearance,
//! commands and settings, and programs: helpers run beside the app, and commands they provide
//! (`sshremote web`, `mcp`, `remote`) in builds without them. See docs/extensions.md.
//!
//! extension.json (`api` is the extension API it needs, at most [`API`]):
//!   { "id": "dev.sshremote.ocean", "name": "Ocean", "version": "1.0.0", "api": 1,
//!     "description": "...", "contributes": { "layers": [
//!       { "file": "sea.json", "label": "Ocean: the sea", "still": true } ] } }
//!
//! The registry is a static index (registry/build.py writes it):
//!   { "extensions": [ { "id", "name", "version", "description", "url", "sha256", "size", "premium", "provides" } ] }

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(feature = "gui")]
use eframe::egui::{self, RichText};

#[cfg(feature = "gui")]
use crate::gui::ic;

/// What extensions this program can read.
pub const API: u64 = 2;
/// Where the registry's index is unless a setting names another (a company's own mirror).
pub const REGISTRY: &str = "https://sshremote.dev/registry/v1/index.json";

#[derive(Clone, Debug, PartialEq)]
pub struct Manifest {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub layers: Vec<LayerDef>,
    /// The script (WebAssembly), its commands (id, label) and what it may do.
    pub main: Option<String>,
    pub commands: Vec<(String, String)>,
    /// Keys that run commands: (command id, "Ctrl+Alt+T").
    pub keys: Vec<(String, String)>,
    /// What can be set for it on the Extensions page.
    pub settings: Vec<Setting>,
    pub permissions: Vec<String>,
    /// Its own programs, by platform ("windows-x86_64"), and the commands they serve ("web").
    pub programs: Vec<(String, String)>,
    pub provides: Vec<String>,
    /// "startup": its program runs beside the app, talking with the extension host.
    pub activation: Option<String>,
}

/// A setting an extension declares: its id, what it is called, its kind ("text", "secret",
/// "bool", "number") and its value until changed.
#[derive(Clone, Debug, PartialEq)]
pub struct Setting {
    pub id: String,
    pub label: String,
    pub kind: String,
    pub default: String,
}

/// An extension's setting as it is now; a secret one is kept by the system's password store.
pub fn setting(ext: &str, s: &Setting) -> String {
    if s.kind == "secret" {
        return crate::vault::get(&secret_name(ext, &s.id)).unwrap_or_default();
    }
    crate::hosts::setting(&format!("ext.{ext}.{}", s.id)).unwrap_or_else(|| s.default.clone())
}

pub fn set_setting(ext: &str, s: &Setting, value: &str) -> Result<(), String> {
    if s.kind == "secret" {
        return crate::vault::set(&secret_name(ext, &s.id), value);
    }
    crate::hosts::set_setting(&format!("ext.{ext}.{}", s.id), &value.replace(['\n', '\r'], " "));
    Ok(())
}

fn secret_name(ext: &str, id: &str) -> String {
    format!("sshremote-extension:{ext}:{id}")
}

/// All of an extension's settings as JSON, as it is told them.
pub fn settings_json(m: &Manifest) -> serde_json::Value {
    let map: serde_json::Map<String, serde_json::Value> = m
        .settings
        .iter()
        .map(|s| {
            let v = setting(&m.id, s);
            let v = match s.kind.as_str() {
                "bool" => serde_json::json!(v == "true"),
                "number" => v.parse::<f64>().map_or(serde_json::json!(v), |n| serde_json::json!(n)),
                _ => serde_json::json!(v),
            };
            (s.id.clone(), v)
        })
        .collect();
    serde_json::Value::Object(map)
}

/// A background layer an extension brings.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerDef {
    pub file: String,
    pub label: String,
    pub still: bool,
}

/// An extension the registry offers.
#[derive(Clone, Debug, PartialEq)]
pub struct Offer {
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    pub url: String,
    pub sha256: String,
    /// Only with a premium license.
    pub premium: bool,
    /// Commands its program serves ("remote", "web").
    pub provides: Vec<String>,
}

impl Offer {
    /// Whether this sshremote already has what it brings.
    pub fn built_in(&self) -> bool {
        !self.provides.is_empty() && self.provides.iter().all(|p| crate::modules::built_in(p))
    }
}

fn root() -> Option<PathBuf> {
    #[cfg(test)]
    if let Some(r) = TEST_ROOT.with(|r| r.borrow().clone()) {
        return Some(r);
    }
    crate::hosts::config_dir().map(|d| d.join("extensions"))
}

#[cfg(test)]
thread_local!(static TEST_REGISTRY: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) });
#[cfg(test)]
thread_local!(static TEST_ROOT: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) });

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 100 && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn parse_manifest(text: &str) -> Result<Manifest, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("extension.json: {e}"))?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).unwrap_or("").to_string();
    let api = v.get("api").and_then(|x| x.as_u64()).unwrap_or(1);
    if api > API {
        return Err(format!("it needs a newer sshremote (extension API {api}, this one reads {API})"));
    }
    let id = s("id");
    if !valid_id(&id) {
        return Err("extension.json: its id must be letters, digits, dots, dashes".into());
    }
    let layers = v
        .pointer("/contributes/layers")
        .and_then(|x| x.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|l| {
                    let file = l.get("file")?.as_str()?.to_string();
                    // Files stay inside the extension's folder.
                    if file.contains("..") || file.starts_with('/') || file.contains(':') {
                        return None;
                    }
                    Some(LayerDef {
                        label: l.get("label").and_then(|x| x.as_str()).unwrap_or(&file).to_string(),
                        still: l.get("still").and_then(|x| x.as_bool()).unwrap_or(false),
                        file,
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let main = v.get("main").and_then(|x| x.as_str()).filter(|f| !f.contains("..") && !f.starts_with('/') && !f.contains(':')).map(str::to_string);
    let commands = v
        .pointer("/contributes/commands")
        .and_then(|x| x.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let id = c.get("id")?.as_str()?;
                    let ok = !id.is_empty() && id.len() <= 60 && id.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
                    ok.then(|| (id.to_string(), c.get("label").and_then(|x| x.as_str()).unwrap_or(id).to_string()))
                })
                .collect()
        })
        .unwrap_or_default();
    let word = |s: &str| !s.is_empty() && s.len() <= 60 && s.chars().all(|ch| ch.is_ascii_alphanumeric() || ch == '_');
    let keys = v
        .pointer("/contributes/commands")
        .and_then(|x| x.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|c| Some((c.get("id")?.as_str()?.to_string(), c.get("key")?.as_str()?.to_string())))
                .filter(|(id, k)| word(id) && crate::ext::parse_key(k).is_some())
                .collect()
        })
        .unwrap_or_default();
    let settings = v
        .pointer("/contributes/settings")
        .and_then(|x| x.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|c| {
                    let id = c.get("id")?.as_str()?.to_string();
                    let kind = c.get("type").and_then(|x| x.as_str()).unwrap_or("text").to_string();
                    let default = match c.get("default") {
                        Some(serde_json::Value::String(s)) => s.clone(),
                        Some(serde_json::Value::Null) | None => String::new(),
                        Some(other) => other.to_string(),
                    };
                    let ok = word(&id) && ["text", "secret", "bool", "number"].contains(&kind.as_str());
                    ok.then(|| Setting { label: c.get("label").and_then(|x| x.as_str()).unwrap_or(&id).to_string(), id, kind, default })
                })
                .collect()
        })
        .unwrap_or_default();
    let permissions: Vec<String> = v.get("permissions").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if let Some(p) = permissions.iter().find(|p| !crate::script::PERMISSIONS.iter().any(|(k, _)| k == p)) {
        return Err(format!("extension.json asks for a permission this sshremote does not know: {p}"));
    }
    let inside = |f: &str| !f.is_empty() && !f.contains("..") && !f.starts_with('/') && !f.contains(':') && !f.contains('\\');
    let programs: Vec<(String, String)> = v
        .get("programs")
        .and_then(|x| x.as_object())
        .map(|o| o.iter().filter_map(|(k, f)| f.as_str().filter(|f| inside(f)).map(|f| (k.clone(), f.to_string()))).collect())
        .unwrap_or_default();
    let provides: Vec<String> = v
        .pointer("/contributes/provides")
        .and_then(|x| x.as_array())
        .map(|a| a.iter().filter_map(|p| p.as_str()).filter(|p| *p == "remote" || crate::modules::ALL.iter().any(|m| m.0 == *p)).map(str::to_string).collect())
        .unwrap_or_default();
    if !programs.is_empty() && !permissions.iter().any(|p| p == "native") {
        return Err("extension.json brings programs without the \"native\" permission".into());
    }
    let activation = v.get("activation").and_then(|x| x.as_str()).map(str::to_string);
    if activation.is_some() && programs.is_empty() {
        return Err("extension.json has an activation but no program to run".into());
    }
    Ok(Manifest {
        keys,
        settings,
        activation,
        programs,
        provides,
        name: if s("name").is_empty() { id.clone() } else { s("name") },
        id,
        version: s("version"),
        description: s("description"),
        layers,
        main,
        commands,
        permissions,
    })
}

/// The extensions installed, by name.
pub fn installed() -> Vec<Manifest> {
    let Some(root) = root() else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Manifest> = rd
        .flatten()
        // Not the versions kept for a roll-back, nor ones half unpacked.
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| std::fs::read_to_string(e.path().join("extension.json")).ok())
        .filter_map(|t| parse_manifest(&t).ok())
        .collect();
    out.sort_by_key(|m| m.name.to_lowercase());
    out
}

/// Installs a package: checks it, unpacks it beside the others, and puts it in place of an older
/// version in one step. Its manifest.
pub fn install_file(package: &Path) -> Result<Manifest, String> {
    let bytes = std::fs::read(package).map_err(|e| format!("{}: {e}", package.display()))?;
    install_bytes(&bytes)
}

pub fn install_bytes(bytes: &[u8]) -> Result<Manifest, String> {
    let mut zip = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| format!("not an extension package: {e}"))?;
    let mut text = String::new();
    zip.by_name("extension.json").map_err(|_| "the package has no extension.json".to_string())?.read_to_string(&mut text).map_err(|e| e.to_string())?;
    let m = parse_manifest(&text)?;
    let root = root().ok_or("no settings folder")?;
    let fresh = root.join(format!(".{}.new", m.id));
    let _ = std::fs::remove_dir_all(&fresh);
    for i in 0..zip.len() {
        let mut f = zip.by_index(i).map_err(|e| e.to_string())?;
        // Only paths inside the folder (zip-slip refused).
        let Some(name) = f.enclosed_name() else { continue };
        let to = fresh.join(name);
        if f.is_dir() {
            std::fs::create_dir_all(&to).map_err(|e| e.to_string())?;
            continue;
        }
        if let Some(p) = to.parent() {
            std::fs::create_dir_all(p).map_err(|e| e.to_string())?;
        }
        let mut out = std::fs::File::create(&to).map_err(|e| e.to_string())?;
        std::io::copy(&mut f, &mut out).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        if m.programs.iter().any(|(_, p)| fresh.join(p) == to) {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&to, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
    }
    #[cfg(not(test))]
    crate::exthost::stop(&m.id);
    let place = root.join(&m.id);
    let old = root.join(format!(".{}.old", m.id));
    let _ = std::fs::remove_dir_all(&old);
    if place.exists() {
        std::fs::rename(&place, &old).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&fresh, &place).map_err(|e| e.to_string())?;
    changed_id(&m.id);
    Ok(m)
}

/// A key as extensions name it ("Ctrl+Alt+T", "Ctrl+Shift+F5"): its modifiers and its key.
pub fn parse_key(text: &str) -> Option<(u8, String)> {
    let mut mods = 0u8;
    let mut key = None;
    for part in text.split('+').map(str::trim) {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => mods |= 1,
            "shift" => mods |= 2,
            "alt" => mods |= 4,
            k if key.is_none() && !k.is_empty() => key = Some(k.to_string()),
            _ => return None,
        }
    }
    // A key that types nothing alone (letters need Ctrl or Alt), so typing is never taken.
    let key = key?;
    let plain_ok = key.starts_with('f') && key.len() > 1 && key[1..].parse::<u8>().is_ok();
    (mods & 5 != 0 || plain_ok).then_some((mods, key))
}

/// The files `ext new` writes, from assets/extension-template.
const TEMPLATE: [(&str, &str); 5] = [
    ("extension.json", include_str!("../assets/extension-template/extension.json")),
    ("Cargo.toml", include_str!("../assets/extension-template/Cargo.toml")),
    ("src/main.rs", include_str!("../assets/extension-template/src/main.rs")),
    ("README.md", include_str!("../assets/extension-template/README.md")),
    (".gitignore", include_str!("../assets/extension-template/gitignore.txt")),
];

/// A new extension in `dir`, named after the folder: a Rust program with a status bar item, a
/// command with a key, a panel, a setting and a form.
pub fn new_extension(dir: &Path) -> Result<Manifest, String> {
    if dir.exists() && std::fs::read_dir(dir).map_err(|e| e.to_string())?.next().is_some() {
        return Err(format!("{} is not empty", dir.display()));
    }
    let folder = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let bin: String = folder.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect::<String>().trim_matches('-').to_string();
    if bin.is_empty() || !bin.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return Err("name the folder with letters first, as my-extension".into());
    }
    let name: String = bin.split('-').filter(|w| !w.is_empty()).map(|w| w[..1].to_uppercase() + &w[1..]).collect::<Vec<_>>().join(" ");
    let id = format!("local.{bin}");
    for (file, text) in TEMPLATE {
        let to = dir.join(file);
        if let Some(d) = to.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let text = text.replace("EXT_ID", &id).replace("EXT_NAME", &name).replace("EXT_BIN", &bin);
        std::fs::write(&to, text).map_err(|e| format!("{}: {e}", to.display()))?;
    }
    parse_manifest(&std::fs::read_to_string(dir.join("extension.json")).map_err(|e| e.to_string())?)
}

/// A Rust extension (a Cargo.toml beside it) built for this computer, its program put where
/// extension.json names it; anything else is left as it is.
fn build(dir: &Path) -> Result<(), String> {
    let cargo = dir.join("Cargo.toml");
    if !cargo.is_file() {
        return Ok(());
    }
    let m = parse_manifest(&std::fs::read_to_string(dir.join("extension.json")).map_err(|e| format!("extension.json: {e}"))?)?;
    let here = platform();
    let Some((_, program)) = m.programs.iter().find(|(p, _)| *p == here) else { return Ok(()) };
    let text = std::fs::read_to_string(&cargo).map_err(|e| e.to_string())?;
    let bin = text.lines().find_map(|l| l.trim().strip_prefix("name").map(|r| r.trim_start().trim_start_matches('=').trim().trim_matches('"').to_string())).ok_or("Cargo.toml has no name")?;
    println!("building {bin}...");
    let ok = std::process::Command::new("cargo").args(["build", "--release", "--quiet"]).current_dir(dir).status().map_err(|e| format!("cargo: {e}"))?;
    if !ok.success() {
        return Err("the build failed (see above)".into());
    }
    let exe = dir.join("target").join("release").join(format!("{bin}{}", std::env::consts::EXE_SUFFIX));
    std::fs::copy(&exe, dir.join(program)).map_err(|e| format!("{}: {e}", exe.display()))?;
    Ok(())
}

/// What a dev folder's sources look like: its files and those under src/, but not what the build
/// makes (target/, the programs, Cargo.lock), which would start it again.
fn source_stamp(dir: &Path) -> String {
    let programs: Vec<String> = parse_manifest(&std::fs::read_to_string(dir.join("extension.json")).unwrap_or_default())
        .map(|m| m.programs.into_iter().map(|(_, f)| f).collect())
        .unwrap_or_default();
    // Cargo writes its lock file as it builds.
    let made = |e: &&str| e.starts_with("Cargo.lock:") || programs.iter().any(|p| e.starts_with(&format!("{p}:")));
    let mut out = folder_stamp(dir).split(',').filter(|e| !made(e)).collect::<Vec<_>>().join(",");
    let mut stack = vec![dir.join("src")];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            if e.path().is_dir() {
                stack.push(e.path());
            }
        }
        out.push('|');
        out.push_str(&folder_stamp(&d));
    }
    out
}

/// The times and sizes of the files in a folder (not its subfolders), to notice a change.
fn folder_stamp(dir: &Path) -> String {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok().filter(|m| m.is_file())?;
            let t = m.modified().ok()?.duration_since(std::time::UNIX_EPOCH).ok()?.as_millis();
            Some(format!("{}:{t}:{}", e.file_name().to_string_lossy(), m.len()))
        })
        .collect();
    v.sort();
    v.join(",")
}

/// An installed extension's folder.
pub fn folder(id: &str) -> Option<PathBuf> {
    valid_id(id).then(|| root().map(|r| r.join(id))).flatten()
}

/// This computer's platform as extensions name it.
pub fn platform() -> String {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        o => o,
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// The installed program that serves a command (`web`, `mcp`) here, and its extension's name.
pub fn provider(cmd: &str) -> Option<(PathBuf, String)> {
    let root = root()?;
    let here = platform();
    installed().into_iter().find_map(|m| {
        let file = m.programs.iter().find(|(p, _)| *p == here)?.1.clone();
        m.provides.iter().any(|p| p == cmd).then(|| (root.join(&m.id).join(file), m.name.clone()))
    })
}

/// Runs a command through the extension that serves it, as if built in; its exit code is ours.
#[cfg_attr(all(feature = "web", feature = "mcp"), allow(dead_code))]
pub fn run_provided(cmd: &str, args: &[String]) -> Result<(), String> {
    let (program, name) = provider(cmd).ok_or_else(|| missing(cmd))?;
    let status = std::process::Command::new(&program).arg(cmd).args(args).status().map_err(|e| format!("{name}: {}: {e}", program.display()))?;
    match status.code() {
        Some(0) => Ok(()),
        Some(c) => std::process::exit(c),
        None => Err(format!("{name} stopped")),
    }
}

/// Runs a command of the extension that serves `cmd` and waits; its error output if it fails.
#[cfg_attr(feature = "remote", allow(dead_code))]
pub fn call_provided(cmd: &str, args: &[String]) -> Result<(), String> {
    let (program, name) = provider(cmd).ok_or_else(|| missing(cmd))?;
    let out = std::process::Command::new(&program).args(args).output().map_err(|e| format!("{name}: {e}"))?;
    if out.status.success() {
        return Ok(());
    }
    Err(String::from_utf8_lossy(&out.stderr).trim().trim_start_matches("sshremote: ").to_string())
}

/// Starts a command of the extension that serves `cmd` (a window of its own) without waiting.
#[cfg_attr(feature = "remote", allow(dead_code))]
pub fn spawn_provided(cmd: &str, args: &[String]) -> Result<(), String> {
    let (program, name) = provider(cmd).ok_or_else(|| missing(cmd))?;
    std::process::Command::new(&program).args(args).spawn().map(|_| ()).map_err(|e| format!("{name}: {e}"))
}

/// What to install for a command neither built in nor installed.
pub fn missing(cmd: &str) -> String {
    let (name, id) = match cmd {
        "web" => ("The web gateway comes", "dev.sshremote.web-gateway"),
        "mcp" => ("The MCP server comes", "dev.sshremote.mcp-server"),
        _ => ("Remote screens, files and the agent come", "dev.sshremote.remote-desktop"),
    };
    format!("{name} with an extension: sshremote ext install {id}, or Extensions in the open dialog")
}

/// Removes an installed extension (its layers in use show nothing from then on).
pub fn remove(id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("no such extension".into());
    }
    let root = root().ok_or("no settings folder")?;
    #[cfg(not(test))]
    crate::exthost::stop(id);
    std::fs::remove_dir_all(root.join(id)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(root.join(format!(".{id}.old")));
    changed_id(id);
    Ok(())
}

/// Puts the version an update replaced back.
pub fn roll_back(id: &str) -> Result<(), String> {
    let root = root().ok_or("no settings folder")?;
    let old = root.join(format!(".{id}.old"));
    if !valid_id(id) || !old.exists() {
        return Err("no earlier version kept".into());
    }
    #[cfg(not(test))]
    crate::exthost::stop(id);
    let place = root.join(id);
    let _ = std::fs::remove_dir_all(&place);
    std::fs::rename(old, place).map_err(|e| e.to_string())?;
    changed_id(id);
    Ok(())
}

/// Makes a package from a folder with extension.json.
pub fn pack(dir: &Path, out: &Path) -> Result<Manifest, String> {
    let m = parse_manifest(&std::fs::read_to_string(dir.join("extension.json")).map_err(|e| format!("extension.json: {e}"))?)?;
    if let Some(main) = &m.main
        && !dir.join(main).is_file()
    {
        return Err(format!("{main} is named in extension.json but not there"));
    }
    for l in &m.layers {
        if !dir.join(&l.file).is_file() {
            return Err(format!("{} is named in extension.json but not there", l.file));
        }
    }
    // Only what the manifest names goes in: not sources, build folders or leftovers.
    let mut named = vec!["extension.json".to_string()];
    named.extend(m.main.clone());
    named.extend(m.layers.iter().map(|l| l.file.clone()));
    let programs: Vec<String> = m.programs.iter().map(|(_, f)| f.clone()).filter(|f| dir.join(f).is_file()).collect();
    if !m.programs.is_empty() && programs.is_empty() {
        return Err("none of the programs named in extension.json is there: build one first".into());
    }
    named.extend(programs);
    named.sort();
    named.dedup();
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    for f in &named {
        zip.start_file(f.as_str(), opts).map_err(|e| e.to_string())?;
        zip.write_all(&std::fs::read(dir.join(f)).map_err(|e| format!("{f}: {e}"))?).map_err(|e| e.to_string())?;
    }
    zip.finish().map_err(|e| e.to_string())?;
    Ok(m)
}

// ----- Layers for Appearance ----------------------------------------------------------------------

/// The background layers installed extensions bring: their sources (`ext:ID/FILE`) and names.
pub fn layers() -> Vec<(String, String, bool)> {
    let mut cache = LAYERS.lock().unwrap_or_else(|e| e.into_inner());
    cache.get_or_insert_with(read_layers).clone()
}

/// The extension last installed, updated or removed, for the host to run again as it now is.
static CHANGED_ID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

fn changed_id(id: &str) {
    *CHANGED_ID.lock().unwrap_or_else(|e| e.into_inner()) = Some(id.to_string());
    changed();
}

/// Read once; installing and removing read again.
static LAYERS: std::sync::Mutex<Option<Vec<(String, String, bool)>>> = std::sync::Mutex::new(None);

fn changed() {
    *LAYERS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    #[cfg(not(test))]
    if let Some(id) = CHANGED_ID.lock().unwrap_or_else(|e| e.into_inner()).take() {
        crate::exthost::reload(&id);
    }
    UPDATES.lock().unwrap_or_else(|e| e.into_inner()).1 = false;
}

fn read_layers() -> Vec<(String, String, bool)> {
    installed()
        .into_iter()
        .flat_map(|m| m.layers.into_iter().map(move |l| (format!("ext:{}/{}", m.id, l.file), l.label, l.still)))
        .collect()
}

/// An extension layer's file on disk, for an `ext:` source; anything else as it is.
pub fn resolve(src: &str) -> String {
    match src.strip_prefix("ext:").and_then(|r| r.split_once('/')) {
        Some((id, file)) if valid_id(id) && !file.contains("..") => {
            root().map_or_else(|| src.to_string(), |r| r.join(id).join(file).to_string_lossy().into_owned())
        }
        _ => src.to_string(),
    }
}

/// Whether an extension layer is a still one (drawn once).
pub fn is_still(src: &str) -> bool {
    src.starts_with("ext:") && layers().iter().any(|(s, _, still)| s == src && *still)
}

/// An extension layer's name, if it is one.
pub fn label(src: &str) -> Option<String> {
    src.starts_with("ext:").then(|| layers().into_iter().find(|(s, ..)| s == src).map(|(_, l, _)| l)).flatten()
}

// ----- The registry ---------------------------------------------------------------------------------

pub fn registry_url() -> String {
    #[cfg(test)]
    if let Some(u) = TEST_REGISTRY.with(|r| r.borrow().clone()) {
        return u;
    }
    crate::hosts::setting("registry_url").filter(|u| !u.trim().is_empty()).unwrap_or_else(|| REGISTRY.to_string())
}

/// Fetches with the system's curl (on Windows 10 and later, Linux and macOS alike); file paths
/// and file:// URLs are read directly.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if let Some(p) = url.strip_prefix("file://") {
        return std::fs::read(p.trim_start_matches('/').replace("%20", " ")).or_else(|_| std::fs::read(p)).map_err(|e| e.to_string());
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return std::fs::read(url).map_err(|e| e.to_string());
    }
    let mut cmd = std::process::Command::new("curl");
    cmd.args(["-fsSL", "--max-time", "120", "--proto", "=https,http", url]);
    crate::sys::quiet(&mut cmd);
    let out = cmd.output().map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("could not fetch {url}: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    Ok(out.stdout)
}

/// The key that signs the registry's index (ed25519, checked with OpenSSH's ssh-keygen).
pub const SIGNER: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIFgMJKE5ZmC2xmWV5LZMjrpbZ3d9JoC0GJX0QNUEGsQX";
const SIGNER_ID: &str = "registry@sshremote.dev";
const NAMESPACE: &str = "sshremote-registry";

/// Checks the index against its signature with `ssh-keygen -Y verify`.
fn verify(data: &[u8], sig: &[u8]) -> Result<(), String> {
    verify_with(data, sig, SIGNER, NAMESPACE).map_err(|e| format!("the registry's index is not signed by sshremote.dev; nothing installed ({e})"))
}

/// Checks `data` against an ssh signature by `key` in `namespace`.
pub fn verify_with(data: &[u8], sig: &[u8], key: &str, namespace: &str) -> Result<(), String> {
    // In this user's own settings folder: in the shared temp folder another user could put
    // their own allowed signers in its place.
    let base = crate::hosts::config_dir().ok_or("no settings folder")?;
    let dir = base.join(format!("verify-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&base).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir).map_err(|e| e.to_string())?;
    let signers = dir.join("allowed_signers");
    let sig_file = dir.join("index.sig");
    let r = (|| {
        std::fs::write(&signers, format!("{SIGNER_ID} namespaces=\"{namespace}\" {key}
")).map_err(|e| e.to_string())?;
        std::fs::write(&sig_file, sig).map_err(|e| e.to_string())?;
        let mut cmd = std::process::Command::new("ssh-keygen");
        cmd.args(["-Y", "verify", "-I", SIGNER_ID, "-n", namespace, "-f"]).arg(&signers).arg("-s").arg(&sig_file);
        cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::piped());
        crate::sys::quiet(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("ssh-keygen (OpenSSH) is needed to check the registry: {e}"))?;
        child.stdin.take().ok_or("ssh-keygen")?.write_all(data).map_err(|e| e.to_string())?;
        let out = child.wait_with_output().map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("bad signature: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    })();
    let _ = std::fs::remove_file(&signers);
    let _ = std::fs::remove_file(&sig_file);
    let _ = std::fs::remove_dir(&dir);
    r
}

/// What the registry offers.
pub fn offers() -> Result<Vec<Offer>, String> {
    let url = registry_url();
    let index = fetch(&url)?;
    // Over the network the index must carry the registry's signature; it names each package's
    // checksum, so the packages are vouched for with it. A local index (a mirror on disk) is trusted.
    if url.starts_with("https://") || url.starts_with("http://") {
        verify(&index, &fetch(&format!("{url}.sig"))?)?;
    }
    let v: serde_json::Value = serde_json::from_slice(&index).map_err(|e| format!("the registry's index: {e}"))?;
    let base = url.rsplit_once(['/', '\\']).map_or(String::new(), |(b, _)| b.to_string());
    Ok(v.get("extensions")
        .and_then(|x| x.as_array())
        .map(|list| {
            list.iter()
                .filter_map(|e| {
                    let s = |k: &str| e.get(k).and_then(|x| x.as_str()).map(str::to_string);
                    // A native extension is packed for each platform: this one's, or none here.
                    let pkg = match e.get("platforms") {
                        Some(p) => p.get(platform())?,
                        None => e,
                    };
                    let p = |k: &str| pkg.get(k).and_then(|x| x.as_str()).map(str::to_string);
                    let url = p("url")?;
                    Some(Offer {
                        id: s("id").filter(|i| valid_id(i))?,
                        name: s("name").unwrap_or_default(),
                        version: s("version").unwrap_or_default(),
                        description: s("description").unwrap_or_default(),
                        // Relative to the index, as the registry lays packages out beside it.
                        url: if url.contains("://") { url } else { format!("{base}/{url}") },
                        sha256: p("sha256")?.to_lowercase(),
                        premium: e.get("premium").and_then(|x| x.as_bool()).unwrap_or(false),
                        provides: e.get("provides").and_then(|x| x.as_array()).map(|a| a.iter().filter_map(|p| p.as_str().map(str::to_string)).collect()).unwrap_or_default(),
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Installs (or updates) what the registry offers, after checking the package is the one the
/// index names.
pub fn install_offer(o: &Offer) -> Result<Manifest, String> {
    use sha2::Digest;
    // A deliberate soft gate: checked here only, the packages themselves are not locked.
    if o.premium && !crate::license::premium() {
        return Err(format!("{} comes with a premium license: see https://sshremote.dev/premium", o.name));
    }
    let bytes = fetch(&o.url)?;
    let hash: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    if hash != o.sha256 {
        return Err(format!("{} did not arrive as the registry describes it (checksum differs); not installed", o.name));
    }
    install_bytes(&bytes)
}

/// Whether version `a` is newer than `b` (numbers compared part by part).
pub fn newer(a: &str, b: &str) -> bool {
    let parts = |v: &str| v.split(['.', '-']).map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    parts(a) > parts(b)
}

/// How many installed extensions the registry has newer versions of: looked up in the
/// background (again after each install), None until it answers or when it cannot be reached.
#[cfg(feature = "gui")]
pub fn updates_waiting() -> Option<usize> {
    if cfg!(test) {
        return None;
    }
    let mut state = UPDATES.lock().unwrap_or_else(|e| e.into_inner());
    if !state.1 {
        state.1 = true;
        std::thread::spawn(|| {
            let n = offers().ok().map(|o| installed().iter().filter(|m| o.iter().any(|x| x.id == m.id && newer(&x.version, &m.version))).count());
            UPDATES.lock().unwrap_or_else(|e| e.into_inner()).0 = n;
        });
    }
    state.0
}

/// The count of updates waiting, and whether it has been asked for since the last change.
static UPDATES: std::sync::Mutex<(Option<usize>, bool)> = std::sync::Mutex::new((None, false));

/// Updates every installed extension the registry has a newer version of; what was updated.
pub fn update_all() -> Result<Vec<String>, String> {
    let offers = offers()?;
    let mut done = Vec::new();
    for m in installed() {
        if let Some(o) = offers.iter().find(|o| o.id == m.id && newer(&o.version, &m.version)) {
            install_offer(o)?;
            done.push(format!("{} {} \u{2192} {}", m.name, m.version, o.version));
        }
    }
    Ok(done)
}

// ----- The command line ---------------------------------------------------------------------------

/// `sshremote ext list | install FILE-OR-ID | update | remove ID | roll-back ID | pack DIR OUT | available`
pub fn cli(args: &[String]) -> Result<(), String> {
    let arg = |i: usize| args.get(i).map(String::as_str).ok_or("missing argument (see sshremote ext)");
    match args.first().map(String::as_str) {
        Some("list") => {
            for m in installed() {
                println!("{}  {}  {}", m.id, m.version, m.name);
            }
            Ok(())
        }
        // Packs and installs a folder whenever it changes; a running app starts the new version.
        Some("new") => {
            let dir = Path::new(arg(1)?);
            let m = new_extension(dir)?;
            println!("made {} ({}) in {}", m.name, m.id, dir.display());
            println!("next: sshremote ext dev {}", dir.display());
            Ok(())
        }
        Some("dev") => {
            let dir = Path::new(arg(1)?).to_path_buf();
            let out = std::env::temp_dir().join(format!("sshremote-dev-{}.sshx", std::process::id()));
            let mut seen = String::new();
            println!("watching {} (Ctrl+C to stop)", dir.display());
            loop {
                let now = source_stamp(&dir);
                if now != seen {
                    seen = now;
                    match build(&dir).and_then(|()| pack(&dir, &out)).and_then(|_| install_file(&out)) {
                        Ok(m) => println!("installed {} {} at {}", m.name, m.version, crate::license::today()),
                        Err(e) => println!("not installed: {e}"),
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(700));
            }
        }
        Some("available") => {
            for o in offers()? {
                println!("{}  {}  {}{}", o.id, o.version, o.name, if o.built_in() { "  (built in here)" } else { "" });
            }
            Ok(())
        }
        Some("install") => {
            let what = arg(1)?;
            let m = if Path::new(what).is_file() {
                install_file(Path::new(what))?
            } else {
                let o = offers()?.into_iter().find(|o| o.id == what).ok_or(format!("{what}: neither a file nor in the registry"))?;
                install_offer(&o)?
            };
            println!("installed {} {}", m.name, m.version);
            Ok(())
        }
        Some("update") => {
            let done = update_all()?;
            println!("{}", if done.is_empty() { "all up to date".to_string() } else { done.join("\n") });
            Ok(())
        }
        Some("remove") => remove(arg(1)?),
        Some("roll-back") => roll_back(arg(1)?),
        Some("run") => {
            let (ext, id) = (arg(1)?, arg(2)?);
            let c = crate::script::commands().into_iter().find(|c| c.ext == ext && c.id == id).ok_or(format!("{ext} has no command {id}"))?;
            let (text, _) = crate::script::run_command(&c, args.get(3).map_or("", String::as_str), "")?;
            println!("{text}");
            Ok(())
        }
        Some("pack") => {
            let m = pack(Path::new(arg(1)?), Path::new(arg(2)?))?;
            println!("packed {} {}", m.id, m.version);
            Ok(())
        }
        _ => Err("usage: sshremote ext list | available | install FILE-OR-ID | update | remove ID | roll-back ID | run ID COMMAND [TEXT] | pack DIR OUT.sshx | new DIR | dev DIR".into()),
    }
}

// ----- The Extensions page ------------------------------------------------------------------------

/// Which part of the Extensions page is shown.
#[derive(Clone, Copy, Default, PartialEq, Debug)]
#[cfg(feature = "gui")]
pub enum ExtTab {
    #[default]
    Installed,
    Browse,
    Manage,
}

/// The Extensions tool: what is installed and how to use it, what the registry offers, and the
/// rest (a package file, modules, license, registry, the extensions' log).
#[derive(Default)]
#[cfg(feature = "gui")]
pub struct ExtTool {
    pub tab: ExtTab,
    offers: Option<Result<Vec<Offer>, String>>,
    loading: Option<std::sync::mpsc::Receiver<Result<Vec<Offer>, String>>>,
    file: String,
    status: Option<(bool, String)>,
    registry: String,
    license_file: String,
    search: String,
    /// The extensions whose settings are open, and the one asked to be removed.
    settings_open: std::collections::HashSet<String>,
    confirm_remove: Option<String>,
    /// Settings being edited, by extension and setting.
    edits: std::collections::HashMap<(String, String), String>,
    /// The extension a click is about to install or update, and the work running in the
    /// background: which extension (if one) and where its outcome arrives.
    pending: Option<String>,
    working: Option<(Option<String>, std::sync::mpsc::Receiver<Result<String, String>>)>,
}

/// What kind of extension it is, as an icon and a word.
#[cfg(feature = "gui")]
fn kind(m: &Manifest) -> (&'static str, &'static str) {
    if m.activation.is_some() {
        (ic::LIGHTNING, "Runs beside the app")
    } else if !m.programs.is_empty() {
        (ic::TERMINAL, "Program")
    } else if m.main.is_some() {
        (ic::CODE, "Script")
    } else if !m.layers.is_empty() {
        (ic::PALETTE, "Style pack")
    } else {
        (ic::PUZZLE_PIECE, "Extension")
    }
}

/// Where to find what an extension adds, one line each.
#[cfg(feature = "gui")]
pub fn how_to_use(m: &Manifest) -> Vec<String> {
    let mut out = Vec::new();
    for (id, key) in &m.keys {
        let label = m.commands.iter().find(|(c, _)| c == id).map_or(id.as_str(), |(_, l)| l.as_str());
        out.push(format!("{key}: {label}"));
    }
    if !m.commands.is_empty() {
        let names: Vec<&str> = m.commands.iter().map(|(_, l)| l.as_str()).collect();
        let place = if m.main.is_some() { "the command palette (Ctrl+Shift+P) and a terminal's right-click menu" } else { "the command palette (Ctrl+Shift+P)" };
        out.push(format!("In {place}: {}", names.join(", ")));
    }
    if m.activation.is_some() {
        out.push("Shows what it has in the status bar and the side panel".into());
    }
    if !m.provides.is_empty() {
        let cmds: Vec<String> = m.provides.iter().map(|c| format!("sshremote {c}")).collect();
        out.push(format!("Adds {}", cmds.join(", ")));
    }
    if !m.layers.is_empty() {
        out.push("Changes the look while it is installed".into());
    }
    out
}

/// Whether an update replaced a version that can be put back.
pub fn can_roll_back(id: &str) -> bool {
    valid_id(id) && root().is_some_and(|r| r.join(format!(".{id}.old")).exists())
}

/// A rounded box around one extension.
#[cfg(feature = "gui")]
fn card<R>(ui: &mut egui::Ui, body: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let v = ui.visuals();
    egui::Frame::new()
        .fill(v.faint_bg_color)
        .stroke(egui::Stroke::new(1.0, v.widgets.noninteractive.bg_stroke.color))
        .corner_radius(egui::CornerRadius::same(crate::gui::theme::R_LG))
        .inner_margin(egui::Margin::symmetric(12, 10))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            body(ui)
        })
        .inner
}

/// A small word in a tinted box.
#[cfg(feature = "gui")]
fn tag(ui: &mut egui::Ui, text: &str, color: egui::Color32) {
    egui::Frame::new()
        .fill(color.gamma_multiply(0.15))
        .corner_radius(egui::CornerRadius::same(crate::gui::theme::R_SM))
        .inner_margin(egui::Margin::symmetric(5, 1))
        .show(ui, |ui| ui.label(RichText::new(text).size(11.0).color(color)));
}

#[cfg(feature = "gui")]
fn matches(search: &str, texts: &[&str]) -> bool {
    let s = search.trim().to_lowercase();
    s.is_empty() || texts.iter().any(|t| t.to_lowercase().contains(&s))
}

#[cfg(feature = "gui")]
type Act = Option<Box<dyn FnOnce() -> Result<String, String> + Send>>;

#[cfg(feature = "gui")]
impl ExtTool {
    pub fn new() -> Self {
        ExtTool { registry: registry_url(), ..Default::default() }
    }

    fn refresh(&mut self, ctx: &egui::Context) {
        let (tx, rx) = std::sync::mpsc::channel();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(offers());
            ctx.request_repaint();
        });
        self.loading = Some(rx);
    }

    pub fn ui(&mut self, ui: &mut egui::Ui) {
        if self.offers.is_none() && self.loading.is_none() {
            self.refresh(ui.ctx());
        }
        if let Some(rx) = &self.loading
            && let Ok(r) = rx.try_recv()
        {
            self.offers = Some(r);
            self.loading = None;
        }
        let installed = installed();
        let offers: Vec<Offer> = self.offers.as_ref().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default();
        let updates = installed.iter().filter(|m| offers.iter().any(|o| o.id == m.id && newer(&o.version, &m.version))).count();
        // The tabs, and a search over the first two.
        ui.horizontal(|ui| {
            crate::gui::menu_look(ui);
            let installed_name = if updates > 0 { format!("Installed ({})  \u{2022} {updates} update{}", installed.len(), if updates == 1 { "" } else { "s" }) } else { format!("Installed ({})", installed.len()) };
            for (t, glyph, name) in [(ExtTab::Installed, ic::CHECK_CIRCLE, installed_name), (ExtTab::Browse, ic::STOREFRONT, "Browse".to_string()), (ExtTab::Manage, ic::SLIDERS_HORIZONTAL, "Manage".to_string())] {
                ui.selectable_value(&mut self.tab, t, format!("{glyph}  {name}"));
            }
            if self.tab != ExtTab::Manage {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.add(egui::TextEdit::singleline(&mut self.search).hint_text(format!("{}  Search", ic::SEARCH)).desired_width(170.0));
                });
            }
        });
        ui.add_space(6.0);
        if let Some((ok, text)) = self.status.clone() {
            let color = if ok { crate::gui::good() } else { crate::gui::bad() };
            egui::Frame::new()
                .fill(color.gamma_multiply(0.12))
                .corner_radius(egui::CornerRadius::same(crate::gui::theme::R_MD))
                .inner_margin(egui::Margin::symmetric(10, 6))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(if ok { ic::CHECK_CIRCLE } else { ic::X_CIRCLE }).color(color));
                        ui.label(RichText::new(&text).color(color));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.add(egui::Button::new(ic::X).frame_when_inactive(false)).on_hover_text("Dismiss").clicked() {
                                self.status = None;
                            }
                        });
                    });
                });
            ui.add_space(6.0);
        }
        let mut act: Act = None;
        match self.tab {
            ExtTab::Installed => self.installed_tab(ui, &installed, &offers, &mut act),
            ExtTab::Browse => self.browse_tab(ui, &installed, &mut act),
            ExtTab::Manage => self.manage_tab(ui, &mut act),
        }
        // Work runs in the background and the list stays where it is, the card showing its state.
        if let Some(f) = act {
            let (tx, rx) = std::sync::mpsc::channel();
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let _ = tx.send(f());
                ctx.request_repaint();
            });
            self.working = Some((self.pending.take(), rx));
        }
        if let Some((_, rx)) = &self.working {
            match rx.try_recv() {
                Ok(r) => {
                    self.status = Some(match r {
                        Ok(t) => (true, t),
                        Err(e) => (false, e),
                    });
                    self.working = None;
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => self.working = None,
                Err(_) => ui.ctx().request_repaint_after(std::time::Duration::from_millis(200)),
            }
        }
    }

    fn installed_tab(&mut self, ui: &mut egui::Ui, installed: &[Manifest], offers: &[Offer], act: &mut Act) {
        self.parts_card(ui, act);
        ui.add_space(10.0);
        ui.label(RichText::new(format!("Added extensions ({})", installed.len())).strong());
        ui.label(RichText::new("Installed by you from the registry or a package file; each can be removed.").weak().size(12.0));
        ui.add_space(4.0);
        if installed.is_empty() {
            card(ui, |ui| {
                ui.label(RichText::new(format!("{}  No extensions yet", ic::PUZZLE_PIECE)).size(15.0).strong());
                ui.add_space(4.0);
                ui.label("Extensions add looks, commands, status bar items and panels, and features this edition leaves out, as the remote screen.");
                ui.add_space(6.0);
                if ui.add(crate::gui::theme::primary(format!("{}  Browse extensions", ic::STOREFRONT))).clicked() {
                    self.tab = ExtTab::Browse;
                }
            });
            return;
        }
        if offers.iter().any(|o| installed.iter().any(|m| m.id == o.id && newer(&o.version, &m.version))) {
            ui.horizontal(|ui| {
                if ui.button(format!("{}  Update all", ic::DOWNLOAD)).clicked() {
                    *act = Some(Box::new(|| update_all().map(|d| format!("Updated: {}.", d.join(", ")))));
                }
            });
            ui.add_space(4.0);
        }
        let mut shown = 0;
        for m in installed {
            if !matches(&self.search, &[&m.name, &m.id, &m.description]) {
                continue;
            }
            shown += 1;
            let update = offers.iter().find(|o| o.id == m.id && newer(&o.version, &m.version)).cloned();
            card(ui, |ui| {
                let (glyph, what) = kind(m);
                ui.horizontal(|ui| {
                    ui.label(RichText::new(glyph).size(20.0).color(crate::gui::brand()));
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&m.name).size(15.0).strong());
                            ui.label(RichText::new(format!("v{}", m.version)).weak().size(12.0));
                            tag(ui, what, ui.visuals().text_color());
                            if m.activation.is_some() {
                                let on = crate::exthost::running(&m.id);
                                let (c, t) = if on { (crate::gui::good(), "Running") } else { (crate::gui::bad(), "Stopped") };
                                tag(ui, t, c);
                            }
                        });
                        ui.label(RichText::new(&m.id).weak().size(11.0));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if self.confirm_remove.as_deref() == Some(m.id.as_str()) {
                            if ui.button("Keep").clicked() {
                                self.confirm_remove = None;
                            }
                            if ui.add(egui::Button::new(RichText::new(format!("{}  Remove it", ic::TRASH)).color(crate::gui::bad()))).clicked() {
                                let id = m.id.clone();
                                let name = m.name.clone();
                                *act = Some(Box::new(move || remove(&id).map(|_| format!("{name} removed."))));
                                self.confirm_remove = None;
                            }
                        } else {
                            if crate::gui::icon(ui, ic::TRASH, "Remove", false).clicked() {
                                self.confirm_remove = Some(m.id.clone());
                            }
                            if can_roll_back(&m.id) && crate::gui::icon(ui, ic::CLOCK_COUNTER_CLOCKWISE, "Put the earlier version back", false).clicked() {
                                let id = m.id.clone();
                                *act = Some(Box::new(move || roll_back(&id).map(|_| "The earlier version is back.".to_string())));
                            }
                            if m.activation.is_some() && crate::gui::icon(ui, ic::ARROW_CLOCKWISE, "Start it again", false).clicked() {
                                crate::exthost::reload(&m.id);
                            }
                            if !m.settings.is_empty() {
                                let open = self.settings_open.contains(&m.id);
                                if crate::gui::icon(ui, ic::GEAR, "Settings", open).clicked() && !self.settings_open.remove(&m.id) {
                                    self.settings_open.insert(m.id.clone());
                                }
                            }
                            if let Some(o) = update.clone()
                                && ui.add(crate::gui::theme::primary(format!("{}  Update to {}", ic::DOWNLOAD, o.version))).clicked()
                            {
                                *act = Some(Box::new(move || install_offer(&o).map(|m| format!("{} updated to {}.", m.name, m.version))));
                            }
                        }
                    });
                });
                if !m.description.is_empty() {
                    ui.add_space(2.0);
                    ui.label(&m.description);
                }
                let uses = how_to_use(m);
                if !uses.is_empty() {
                    ui.add_space(4.0);
                    ui.label(RichText::new("How to use it").strong().size(12.0));
                    for u in uses {
                        ui.label(RichText::new(format!("\u{2022}  {u}")).size(12.0));
                    }
                }
                if !m.permissions.is_empty() {
                    ui.add_space(4.0);
                    for p in &m.permissions {
                        let text = crate::script::PERMISSIONS.iter().find(|(k, _)| k == p).map_or(p.as_str(), |(_, t)| *t);
                        ui.label(RichText::new(format!("{}  May {}", ic::SHIELD, lower_first(text))).weak().size(12.0));
                    }
                }
                if self.settings_open.contains(&m.id) {
                    ui.add_space(6.0);
                    ui.separator();
                    self.settings_ui(ui, m);
                }
            });
            ui.add_space(6.0);
        }
        if shown == 0 {
            ui.label(RichText::new("Nothing installed matches the search.").weak());
        }
        ui.add_space(4.0);
        if ui.link(format!("{}  Find more extensions", ic::STOREFRONT)).clicked() {
            self.tab = ExtTab::Browse;
        }
    }

    fn settings_ui(&mut self, ui: &mut egui::Ui, m: &Manifest) {
        ui.label(RichText::new(format!("{}  Settings", ic::GEAR)).strong().size(12.0));
        let mut save = false;
        egui::Grid::new(("ext-settings-grid", &m.id)).num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
            for s in &m.settings {
                let key = (m.id.clone(), s.id.clone());
                let v = self.edits.entry(key).or_insert_with(|| setting(&m.id, s));
                ui.label(&s.label);
                match s.kind.as_str() {
                    "bool" => {
                        let mut on = v == "true";
                        if ui.checkbox(&mut on, "").changed() {
                            *v = on.to_string();
                            save = true;
                        }
                    }
                    kind => {
                        let r = ui.add(egui::TextEdit::singleline(v).password(kind == "secret").desired_width(240.0));
                        let r = if kind == "secret" { r.on_hover_text("Kept in the system's password store") } else { r };
                        save |= r.lost_focus();
                    }
                }
                ui.end_row();
            }
        });
        ui.label(RichText::new("Saved when you leave a field; the extension sees the change at once.").weak().size(11.0));
        if save {
            let mut err = None;
            for s in &m.settings {
                if let Some(v) = self.edits.get(&(m.id.clone(), s.id.clone()))
                    && *v != setting(&m.id, s)
                    && let Err(e) = set_setting(&m.id, s, v)
                {
                    err = Some(e);
                }
            }
            crate::exthost::settings_changed(&m.id);
            self.status = Some(match err {
                Some(e) => (false, e),
                None => (true, format!("{} settings saved.", m.name)),
            });
        }
    }

    fn browse_tab(&mut self, ui: &mut egui::Ui, installed: &[Manifest], act: &mut Act) {
        ui.horizontal(|ui| {
            ui.label(RichText::new("From the signed registry").weak().size(12.0));
            if ui.small_button(format!("{}  Look again", ic::ARROWS_CLOCKWISE)).clicked() {
                self.refresh(ui.ctx());
            }
        });
        ui.add_space(4.0);
        let list = match &self.offers {
            None => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(RichText::new("Looking\u{2026}").weak());
                });
                return;
            }
            Some(Err(e)) => {
                card(ui, |ui| {
                    ui.label(RichText::new("The registry could not be reached").strong().color(crate::gui::bad()));
                    ui.label(RichText::new(e).weak().size(12.0));
                    ui.label(RichText::new("Check the connection, or install a package file under Manage.").size(12.0));
                });
                return;
            }
            Some(Ok(list)) => list.clone(),
        };
        if list.is_empty() {
            ui.label(RichText::new("The registry offers nothing yet.").weak());
        }
        let mut shown = 0;
        for o in &list {
            if !matches(&self.search, &[&o.name, &o.id, &o.description]) {
                continue;
            }
            shown += 1;
            let have = installed.iter().find(|m| m.id == o.id);
            card(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ic::PUZZLE_PIECE).size(20.0).color(crate::gui::brand()));
                    ui.vertical(|ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(&o.name).size(15.0).strong());
                            ui.label(RichText::new(format!("v{}", o.version)).weak().size(12.0));
                            if o.premium {
                                tag(ui, "Premium", crate::gui::brand());
                            }
                        });
                        ui.label(RichText::new(&o.id).weak().size(11.0));
                    });
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let locked = o.premium && !crate::license::premium();
                        let busy = self.working.is_some();
                        if self.working.as_ref().is_some_and(|w| w.0.as_deref() == Some(o.id.as_str())) {
                            ui.label(RichText::new(if have.is_some() { "Updating\u{2026}" } else { "Installing\u{2026}" }).weak());
                            ui.spinner();
                            return;
                        }
                        ui.add_enabled_ui(!busy, |ui| match have {
                            Some(m) if newer(&o.version, &m.version) => {
                                if ui.add(crate::gui::theme::primary(format!("{}  Update to {}", ic::DOWNLOAD, o.version))).clicked() {
                                    let o = o.clone();
                                    self.pending = Some(o.id.clone());
                                    *act = Some(Box::new(move || install_offer(&o).map(|m| format!("{} updated to {}.", m.name, m.version))));
                                }
                            }
                            Some(_) => {
                                ui.label(RichText::new(format!("{}  Installed", ic::CHECK)).color(crate::gui::good()));
                            }
                            None if o.built_in() => {
                                ui.label(RichText::new(format!("{}  Built into this edition", ic::CHECK)).weak()).on_hover_text("This app already has what it brings");
                            }
                            None => {
                                let b = ui.add_enabled(!locked, crate::gui::theme::primary(format!("{}  Install", ic::DOWNLOAD)));
                                if b.on_disabled_hover_text("Comes with a premium license (Manage > License)").clicked() {
                                    let o = o.clone();
                                    self.pending = Some(o.id.clone());
                                    *act = Some(Box::new(move || install_offer(&o).map(|m| format!("{} {} installed. How to use it is on its card under Installed.", m.name, m.version))));
                                }
                            }
                        });
                    });
                });
                if !o.description.is_empty() {
                    ui.add_space(2.0);
                    ui.label(&o.description);
                }
                if !o.provides.is_empty() {
                    let cmds: Vec<String> = o.provides.iter().map(|c| format!("sshremote {c}")).collect();
                    ui.label(RichText::new(format!("Adds {}", cmds.join(", "))).weak().size(12.0));
                }
            });
            ui.add_space(6.0);
        }
        if shown == 0 && !list.is_empty() {
            ui.label(RichText::new("Nothing in the registry matches the search.").weak());
        }
    }

    /// What this app is made of: the core, always here; and each part beyond it, built in or
    /// brought by an extension, which can be turned off.
    fn parts_card(&mut self, ui: &mut egui::Ui, act: &mut Act) {
        let offers: Vec<Offer> = self.offers.as_ref().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default();
        card(ui, |ui| {
            ui.label(RichText::new(format!("{}  Parts of this app", ic::SQUARES_FOUR)).strong());
            ui.label(
                RichText::new("Built in: inside the program you downloaded, never removed. Extension: brought by an added extension below. Untick a part to hide it from the menus.")
                    .weak()
                    .size(12.0),
            );
            ui.add_space(4.0);
            egui::Grid::new("parts").num_columns(3).spacing([12.0, 6.0]).show(ui, |ui| {
                ui.add_enabled(false, egui::Checkbox::new(&mut true, "Core"));
                ui.label(RichText::new("Terminals, SSH hosts, the tools, extensions").weak().size(12.0));
                tag(ui, "Built in", crate::gui::good());
                ui.end_row();
                for (id, name, what) in crate::modules::ALL {
                    let mut on = crate::modules::on(id);
                    if ui.checkbox(&mut on, *name).on_hover_text(*what).changed() {
                        crate::modules::set(id, on);
                    }
                    ui.label(RichText::new(what.replace('`', "")).weak().size(12.0));
                    let serves = crate::modules::served_by(id);
                    if crate::modules::built_in(id) {
                        tag(ui, "Built in", crate::gui::good());
                    } else if let Some((_, by)) = provider(serves) {
                        tag(ui, &format!("Extension: {by}"), crate::gui::brand());
                    } else if let Some(o) = offers.iter().find(|o| o.provides.iter().any(|p| p == serves)) {
                        if ui.small_button(format!("{}  Get {}", ic::DOWNLOAD, o.name)).clicked() {
                            let o = o.clone();
                            *act = Some(Box::new(move || install_offer(&o).map(|m| format!("{} {} installed.", m.name, m.version))));
                        }
                    } else {
                        tag(ui, "Needs an extension", crate::gui::bad());
                    }
                    ui.end_row();
                }
            });
        });
    }

    fn manage_tab(&mut self, ui: &mut egui::Ui, act: &mut Act) {
        card(ui, |ui| {
            ui.label(RichText::new(format!("{}  Install a package file", ic::UPLOAD)).strong());
            ui.label(RichText::new("A .sshx file someone gave you, or one you made with sshremote ext pack.").weak().size(12.0));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.file).hint_text("C:\\path\\to\\extension.sshx").desired_width(320.0));
                if ui.add_enabled(!self.file.trim().is_empty(), egui::Button::new("Install")).clicked() {
                    let f = self.file.trim().trim_matches('"').to_string();
                    *act = Some(Box::new(move || install_file(Path::new(&f)).map(|m| format!("{} {} installed. How to use it is shown on its card.", m.name, m.version))));
                }
            });
        });
        ui.add_space(6.0);
        card(ui, |ui| {
            ui.label(RichText::new(format!("{}  License", ic::KEY)).strong());
            match crate::license::current() {
                Some(l) => {
                    ui.label(crate::license::describe(&l));
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new("The free edition.").weak());
                        ui.hyperlink_to("About premium", "https://sshremote.dev/premium");
                    });
                }
            }
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.license_file).hint_text("A license file").desired_width(320.0));
                if ui.add_enabled(!self.license_file.trim().is_empty(), egui::Button::new("Install")).clicked() {
                    let f = self.license_file.trim().trim_matches('"').to_string();
                    *act = Some(Box::new(move || {
                        let text = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
                        crate::license::install(&text).map(|l| format!("License installed: {}", crate::license::describe(&l)))
                    }));
                }
            });
        });
        ui.add_space(6.0);
        card(ui, |ui| {
            ui.label(RichText::new(format!("{}  Registry", ic::GLOBE)).strong());
            ui.label(RichText::new("Where Browse looks: sshremote.dev, or a company's own mirror. Empty puts the default back.").weak().size(12.0));
            ui.horizontal(|ui| {
                ui.add(egui::TextEdit::singleline(&mut self.registry).desired_width(320.0));
                if ui.button("Use").clicked() {
                    crate::hosts::set_setting("registry_url", self.registry.trim());
                    self.registry = registry_url();
                    self.refresh(ui.ctx());
                }
            });
        });
        ui.add_space(6.0);
        card(ui, |ui| {
            ui.label(RichText::new(format!("{}  Make your own", ic::CODE)).strong());
            ui.label(RichText::new("In a terminal: the first makes a Rust extension to start from, the second builds and installs it on every change while this app shows it.").weak().size(12.0));
            for line in ["sshremote ext new my-tool", "sshremote ext dev my-tool"] {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(line).monospace());
                    if crate::gui::icon(ui, ic::COPY, "Copy", false).clicked() {
                        ui.ctx().copy_text(line.to_string());
                    }
                });
            }
        });
        let log = crate::exthost::log();
        if !log.is_empty() {
            ui.add_space(6.0);
            card(ui, |ui| {
                egui::CollapsingHeader::new(RichText::new(format!("{}  What extensions reported", ic::INFO)).strong()).show(ui, |ui| {
                    egui::ScrollArea::vertical().max_height(180.0).stick_to_bottom(true).show(ui, |ui| {
                        for line in log.iter().rev().take(200).rev() {
                            ui.label(RichText::new(line).monospace().size(11.0));
                        }
                    });
                });
            });
        }
    }
}

/// "Run a program" as "run a program", after "May".
#[cfg(feature = "gui")]
fn lower_first(s: &str) -> String {
    let mut c = s.chars();
    c.next().map_or(String::new(), |f| f.to_lowercase().chain(c).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() {
        let dir = std::env::temp_dir().join(format!("sshremote-ext-test-{}", std::process::id()));
        TEST_ROOT.with(|r| *r.borrow_mut() = Some(dir));
    }

    /// A pack made, installed from the file, its layer found; then the registry offers a newer
    /// one (a local index), the update installs it after the checksum, and roll-back and remove
    /// work; a package with a wrong checksum is refused.
    #[test]
    fn packs_install_update_and_go() {
        use sha2::Digest;
        scratch();
        let work = std::env::temp_dir().join(format!("sshremote-ext-work-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&work);
        let make = |version: &str| {
            let d = work.join(format!("src-{version}"));
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("sea.json"), r#"{"v":"5.7.4","fr":24,"ip":0,"op":1,"w":10,"h":10,"layers":[]}"#).unwrap();
            std::fs::write(d.join("extension.json"), format!(r#"{{"id":"dev.sshremote.test-sea","name":"Sea","version":"{version}","api":1,
                "contributes":{{"layers":[{{"file":"sea.json","label":"Sea","still":true}}]}}}}"#)).unwrap();
            let out = work.join(format!("sea-{version}.sshx"));
            pack(&d, &out).unwrap();
            out
        };
        let one = make("1.0.0");
        let m = install_file(&one).unwrap();
        assert_eq!((m.id.as_str(), m.version.as_str()), ("dev.sshremote.test-sea", "1.0.0"));
        let src = "ext:dev.sshremote.test-sea/sea.json";
        assert!(layers().iter().any(|(s, l, still)| s == src && l == "Sea" && *still));
        assert!(Path::new(&resolve(src)).is_file());
        assert!(is_still(src));

        let two = make("1.1.0");
        let bytes = std::fs::read(&two).unwrap();
        let hash: String = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
        let index = work.join("index.json");
        std::fs::write(&index, format!(r#"{{"api":1,"extensions":[{{"id":"dev.sshremote.test-sea","name":"Sea","version":"1.1.0","url":"sea-1.1.0.sshx","sha256":"{hash}"}}]}}"#)).unwrap();
        TEST_REGISTRY.with(|r| *r.borrow_mut() = Some(index.to_string_lossy().into_owned()));
        let offers = offers().unwrap();
        assert_eq!(offers.len(), 1);
        assert_eq!(update_all().unwrap().len(), 1);
        assert_eq!(installed().iter().find(|m| m.id == "dev.sshremote.test-sea").unwrap().version, "1.1.0");
        roll_back("dev.sshremote.test-sea").unwrap();
        assert_eq!(installed().iter().find(|m| m.id == "dev.sshremote.test-sea").unwrap().version, "1.0.0");
        let bad = Offer { sha256: "00".repeat(32), ..offers[0].clone() };
        assert!(install_offer(&bad).unwrap_err().contains("checksum"));
        if !crate::license::premium() {
            let premium = Offer { premium: true, ..offers[0].clone() };
            assert!(install_offer(&premium).unwrap_err().contains("premium license"));
        }
        remove("dev.sshremote.test-sea").unwrap();
        assert!(installed().iter().all(|m| m.id != "dev.sshremote.test-sea"));
    }

    /// A native extension: its program for this platform serves the command it names; programs
    /// without the "native" permission are refused.
    #[test]
    fn native_programs_serve_their_commands() {
        assert!(parse_manifest(r#"{"id":"x.y","programs":{"linux-x86_64":"x"}}"#).unwrap_err().contains("native"));
        let m = parse_manifest(r#"{"id":"x.y","permissions":["native"],"programs":{"a":"../x","b":"ok"},"contributes":{"provides":["web","nope"]}}"#).unwrap();
        assert_eq!((m.programs, m.provides), (vec![("b".to_string(), "ok".to_string())], vec!["web".to_string()]));
        scratch();
        let d = std::env::temp_dir().join(format!("sshremote-ext-native-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("gw"), b"#!/bin/sh
").unwrap();
        let here = platform();
        std::fs::write(d.join("extension.json"), format!(r#"{{"id":"dev.sshremote.test-gw","version":"1","permissions":["native"],"programs":{{"{here}":"gw"}},"contributes":{{"provides":["web"]}}}}"#)).unwrap();
        let pkg = d.with_extension("sshx");
        pack(&d, &pkg).unwrap();
        install_file(&pkg).unwrap();
        let (prog, _) = provider("web").unwrap();
        assert!(prog.is_file() && provider("mcp").is_none());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&prog).unwrap().permissions().mode() & 0o111, 0o111, "made runnable");
        }
        remove("dev.sshremote.test-gw").unwrap();
        assert!(provider("web").is_none());
        // In the registry it is packed for each platform; only this one's is offered here.
        let index = d.join("index.json");
        std::fs::write(&index, format!(r#"{{"api":1,"extensions":[
            {{"id":"dev.sshremote.test-gw","version":"1","platforms":{{"{here}":{{"url":"gw-here.sshx","sha256":"AB"}},"plan9-mips":{{"url":"gw-p9.sshx","sha256":"cd"}}}}}},
            {{"id":"dev.sshremote.test-other","version":"1","platforms":{{"plan9-mips":{{"url":"o.sshx","sha256":"cd"}}}}}}]}}"#)).unwrap();
        TEST_REGISTRY.with(|r| *r.borrow_mut() = Some(index.to_string_lossy().into_owned()));
        let o = offers().unwrap();
        assert_eq!(o.len(), 1, "nothing for another platform");
        assert!(o[0].url.ends_with("gw-here.sshx") && o[0].sha256 == "ab");
        TEST_REGISTRY.with(|r| *r.borrow_mut() = None);
    }

    /// The Extensions page drawn without a window: an installed native extension shows what it
    /// brings and may do, the registry's offers, the modules and the license.
    #[test]
    fn extensions_page_shows_what_each_brings() {
        use eframe::egui::{self, Pos2, Rect, Vec2};
        let root = std::env::temp_dir().join(format!("sshremote-ext-page-root-{}", std::process::id()));
        TEST_ROOT.with(|r| *r.borrow_mut() = Some(root));
        let d = std::env::temp_dir().join(format!("sshremote-ext-page-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("gw"), b"x").unwrap();
        let here = platform();
        std::fs::write(d.join("extension.json"), format!(r#"{{"id":"dev.sshremote.test-page","name":"Page gateway","version":"1.2.0","description":"A gateway.","permissions":["native"],"programs":{{"{here}":"gw"}},"contributes":{{"provides":["web"]}}}}"#)).unwrap();
        let pkg = d.with_extension("sshx");
        pack(&d, &pkg).unwrap();
        install_file(&pkg).unwrap();
        let offer = |id: &str, name: &str, premium| Offer { id: id.into(), name: name.into(), version: "1.0.0".into(), description: "Offered.".into(), url: String::new(), sha256: String::new(), premium, provides: Vec::new() };
        let mut tool = ExtTool { offers: Some(Ok(vec![
            offer("dev.sshremote.test-sea", "Sea pack", false),
            offer("dev.sshremote.test-gold", "Gold pack", true),
            Offer { version: "1.3.0".into(), ..offer("dev.sshremote.test-page", "Page gateway", false) },
            Offer { provides: vec!["remote".into()], ..offer("dev.sshremote.test-rd", "Desk pack", false) },
        ])), ..ExtTool::new() };
        let ctx = egui::Context::default();
        crate::gui::style(&ctx);
        let size = Vec2::new(760.0, 900.0);
        let pages: [(ExtTab, &str, &[&str]); 3] = [
            (ExtTab::Installed, "installed", &["Parts of this app", "Added extensions (1)", "Installed (1)", "1 update", "Page gateway", "Program", "Adds sshremote web", "run its own program", "Update to 1.3.0", "How to use it"]),
            (ExtTab::Browse, "browse", &["Sea pack", "Gold pack", "Premium", "Installed", "Desk pack", "Install"]),
            (ExtTab::Manage, "manage", &["Install a package file", "License", "Registry", "sshremote ext new my-tool"]),
        ];
        for (tab, name, texts) in pages {
            tool.tab = tab;
            let mut snap = crate::gui::snapshot::Snap::default();
            for k in 0..3 {
                let input = egui::RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, size)), time: Some(k as f64 * 0.1), ..Default::default() };
                snap.frame(&ctx, input, |ui| tool.ui(ui));
            }
            for text in texts {
                assert!(!snap.find(text).is_empty(), "{text} is shown on {name}");
            }
            if tab == ExtTab::Browse {
                assert_eq!(!snap.find("Built into this edition").is_empty(), cfg!(feature = "remote"), "the extension a build carries itself");
            }
            if let Some(dir) = std::env::var_os("SSHREMOTE_SNAP_DIR") {
                snap.save(&ctx, size, &std::path::Path::new(&dir).join(format!("extensions_{name}.png")));
            }
        }
        remove("dev.sshremote.test-page").unwrap();
    }

    /// The live registry's signature is checked: good as published, refused once changed.
    #[test]
    #[ignore]
    fn registry_signature() {
        let url = REGISTRY;
        let index = fetch(url).unwrap();
        let sig = fetch(&format!("{url}.sig")).unwrap();
        verify(&index, &sig).unwrap();
        let mut bad = index.clone();
        bad.extend_from_slice(b" ");
        assert!(verify(&bad, &sig).is_err());
    }

    #[test]
    fn keys_and_settings_are_read() {
        assert_eq!(parse_key("Ctrl+Alt+T"), Some((5, "t".into())));
        assert_eq!(parse_key("F5"), Some((0, "f5".into())));
        assert_eq!(parse_key("T"), None, "a key that types is never taken");
        assert_eq!(parse_key("Shift+T"), None);
        let m = parse_manifest(r#"{"id":"x.y","contributes":{"commands":[{"id":"go","label":"Go","key":"Ctrl+Shift+G"},{"id":"no","key":"Q"}],
            "settings":[{"id":"port","label":"Port","type":"number","default":5432},{"id":"token","type":"secret"},{"id":"bad","type":"colour"}]}}"#).unwrap();
        assert_eq!(m.keys, vec![("go".to_string(), "Ctrl+Shift+G".to_string())]);
        let ids: Vec<_> = m.settings.iter().map(|s| (s.id.as_str(), s.kind.as_str(), s.default.as_str())).collect();
        assert_eq!(ids, [("port", "number", "5432"), ("token", "secret", "")]);
    }

    #[test]
    fn versions_and_ids() {
        assert!(newer("1.10.0", "1.9.3") && !newer("1.0.0", "1.0.0") && newer("2", "1.9"));
        assert!(valid_id("dev.sshremote.ocean-2") && !valid_id("../x") && !valid_id("a/b"));
        assert!(parse_manifest(r#"{"id":"x.y","api":9}"#).unwrap_err().contains("newer"));
        let m = parse_manifest(r#"{"id":"x.y","contributes":{"layers":[{"file":"../evil.json"},{"file":"ok.json"}]}}"#).unwrap();
        assert_eq!(m.layers.len(), 1, "files outside the folder are refused");
    }
}
