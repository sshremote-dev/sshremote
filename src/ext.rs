//! Extensions: packages (`.sshx`, a zip with `extension.json` and its files) that add to the
//! program, installed from a file or from the online registry (sshremote.dev), updated and
//! removed. This first kind are style packs: background layers (Lottie animations and pictures)
//! that show in Appearance beside the built-in ones.
//!
//! extension.json:
//!   { "id": "dev.sshremote.ocean", "name": "Ocean", "version": "1.0.0", "api": 1,
//!     "description": "...", "contributes": { "layers": [
//!       { "file": "sea.json", "label": "Ocean: the sea", "still": true } ] } }
//!
//! The registry is a static index (registry/build.py writes it):
//!   { "api": 1, "extensions": [ { "id", "name", "version", "description", "url", "sha256", "size" } ] }

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

#[cfg(feature = "gui")]
use eframe::egui::{self, RichText};

#[cfg(feature = "gui")]
use crate::gui::ic;

/// What extensions this program can read.
pub const API: u64 = 1;
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
    pub permissions: Vec<String>,
    /// Its own programs, by platform ("windows-x86_64"), and the commands they serve ("web").
    pub programs: Vec<(String, String)>,
    pub provides: Vec<String>,
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
        .map(|a| a.iter().filter_map(|p| p.as_str()).filter(|p| crate::modules::ALL.iter().any(|m| m.0 == *p)).map(str::to_string).collect())
        .unwrap_or_default();
    if !programs.is_empty() && !permissions.iter().any(|p| p == "native") {
        return Err("extension.json brings programs without the \"native\" permission".into());
    }
    Ok(Manifest {
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
    let place = root.join(&m.id);
    let old = root.join(format!(".{}.old", m.id));
    let _ = std::fs::remove_dir_all(&old);
    if place.exists() {
        std::fs::rename(&place, &old).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&fresh, &place).map_err(|e| e.to_string())?;
    changed();
    Ok(m)
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

/// What to install for a command neither built in nor installed.
#[cfg_attr(all(feature = "web", feature = "mcp"), allow(dead_code))]
pub fn missing(cmd: &str) -> String {
    let id = match cmd {
        "web" => "dev.sshremote.web-gateway",
        "mcp" => "dev.sshremote.mcp-server",
        _ => "",
    };
    format!("`{cmd}` comes as an extension here: sshremote ext install {id}")
}

/// Removes an installed extension (its layers in use show nothing from then on).
pub fn remove(id: &str) -> Result<(), String> {
    if !valid_id(id) {
        return Err("no such extension".into());
    }
    let root = root().ok_or("no settings folder")?;
    std::fs::remove_dir_all(root.join(id)).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_dir_all(root.join(format!(".{id}.old")));
    changed();
    Ok(())
}

/// Puts the version an update replaced back.
pub fn roll_back(id: &str) -> Result<(), String> {
    let root = root().ok_or("no settings folder")?;
    let old = root.join(format!(".{id}.old"));
    if !valid_id(id) || !old.exists() {
        return Err("no earlier version kept".into());
    }
    let place = root.join(id);
    let _ = std::fs::remove_dir_all(&place);
    std::fs::rename(old, place).map_err(|e| e.to_string())?;
    changed();
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
    let file = std::fs::File::create(out).map_err(|e| e.to_string())?;
    let mut zip = zip::ZipWriter::new(file);
    let opts = zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).map_err(|e| e.to_string())?.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
                continue;
            }
            let rel = p.strip_prefix(dir).map_err(|e| e.to_string())?.to_string_lossy().replace('\\', "/");
            zip.start_file(rel, opts).map_err(|e| e.to_string())?;
            zip.write_all(&std::fs::read(&p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())?;
        }
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

/// Read once; installing and removing read again.
static LAYERS: std::sync::Mutex<Option<Vec<(String, String, bool)>>> = std::sync::Mutex::new(None);

fn changed() {
    *LAYERS.lock().unwrap_or_else(|e| e.into_inner()) = None;
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
    let dir = std::env::temp_dir().join(format!("sshremote-verify-{}-{:?}", std::process::id(), std::thread::current().id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
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
        Some("available") => {
            for o in offers()? {
                println!("{}  {}  {}", o.id, o.version, o.name);
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
        _ => Err("usage: sshremote ext list | available | install FILE-OR-ID | update | remove ID | roll-back ID | run ID COMMAND [TEXT] | pack DIR OUT.sshx".into()),
    }
}

// ----- The Extensions page ------------------------------------------------------------------------

/// The Extensions tool: what is installed, what the registry offers, installing from a file.
#[derive(Default)]
#[cfg(feature = "gui")]
pub struct ExtTool {
    offers: Option<Result<Vec<Offer>, String>>,
    loading: Option<std::sync::mpsc::Receiver<Result<Vec<Offer>, String>>>,
    file: String,
    status: Option<(bool, String)>,
    registry: String,
    license_file: String,
}

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
        if let Some((ok, text)) = &self.status {
            ui.colored_label(if *ok { crate::gui::brand() } else { crate::gui::bad() }, text);
        }
        let installed = installed();
        ui.label(RichText::new("Installed").strong());
        if installed.is_empty() {
            ui.label(RichText::new("None yet.").weak());
        }
        let offers: Vec<Offer> = self.offers.as_ref().and_then(|r| r.as_ref().ok()).cloned().unwrap_or_default();
        let mut act: Option<Box<dyn FnOnce() -> Result<String, String>>> = None;
        for m in &installed {
            ui.horizontal(|ui| {
                ui.label(RichText::new(&m.name).strong());
                ui.label(RichText::new(format!("{}  {}", m.version, m.id)).weak().size(12.0));
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.button(format!("{}  Remove", ic::TRASH)).clicked() {
                        let id = m.id.clone();
                        act = Some(Box::new(move || remove(&id).map(|_| "Removed.".to_string())));
                    }
                    if let Some(o) = offers.iter().find(|o| o.id == m.id && newer(&o.version, &m.version)) {
                        let o = o.clone();
                        if ui.button(format!("{}  Update to {}", ic::DOWNLOAD, o.version)).clicked() {
                            act = Some(Box::new(move || install_offer(&o).map(|m| format!("{} updated to {}.", m.name, m.version))));
                        }
                    }
                });
            });
            if !m.description.is_empty() {
                ui.label(RichText::new(&m.description).weak().size(12.0));
            }
            if !m.commands.is_empty() {
                let names: Vec<&str> = m.commands.iter().map(|(_, l)| l.as_str()).collect();
                ui.label(RichText::new(format!("Commands (a terminal's right-click menu): {}", names.join(", "))).weak().size(12.0));
            }
            if !m.provides.is_empty() {
                let cmds: Vec<String> = m.provides.iter().map(|c| format!("sshremote {c}")).collect();
                ui.label(RichText::new(format!("Brings: {}", cmds.join(", "))).weak().size(12.0));
            }
            for p in &m.permissions {
                let text = crate::script::PERMISSIONS.iter().find(|(k, _)| k == p).map_or(p.as_str(), |(_, t)| *t);
                ui.label(RichText::new(format!("May: {text}")).weak().size(12.0));
            }
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("From the registry").strong());
            if ui.small_button(ic::ARROWS_CLOCKWISE).on_hover_text("Look again").clicked() {
                self.refresh(ui.ctx());
            }
        });
        match &self.offers {
            None => {
                ui.label(RichText::new("Looking\u{2026}").weak());
            }
            Some(Err(e)) => {
                ui.label(RichText::new(format!("The registry could not be reached: {e}")).weak());
            }
            Some(Ok(list)) if list.is_empty() => {
                ui.label(RichText::new("It offers nothing yet.").weak());
            }
            Some(Ok(list)) => {
                for o in list {
                    let have = installed.iter().find(|m| m.id == o.id);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&o.name).strong());
                        ui.label(RichText::new(&o.version).weak().size(12.0));
                        if o.premium {
                            ui.label(RichText::new("Premium").color(crate::gui::brand()).size(12.0));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if have.is_none() && ui.button(format!("{}  Install", ic::DOWNLOAD)).clicked() {
                                let o = o.clone();
                                act = Some(Box::new(move || install_offer(&o).map(|m| format!("{} {} installed.", m.name, m.version))));
                            }
                        });
                    });
                    if !o.description.is_empty() {
                        ui.label(RichText::new(&o.description).weak().size(12.0));
                    }
                }
            }
        }
        ui.add_space(8.0);
        ui.label(RichText::new("From a file").strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.file).hint_text("A package (.sshx) on this computer").desired_width(320.0));
            if ui.add_enabled(!self.file.trim().is_empty(), egui::Button::new("Install")).clicked() {
                let f = self.file.trim().to_string();
                act = Some(Box::new(move || install_file(Path::new(&f)).map(|m| format!("{} {} installed.", m.name, m.version))));
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Modules").strong());
        ui.label(RichText::new("Parts of the app beyond the terminal; one turned off leaves the menus.").weak().size(12.0));
        crate::modules::ui(ui);
        ui.add_space(8.0);
        ui.label(RichText::new("License").strong());
        match crate::license::current() {
            Some(l) => {
                ui.label(crate::license::describe(&l));
            }
            None => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("The free edition.").weak());
                    ui.hyperlink_to("Premium", "https://sshremote.dev/premium");
                });
            }
        }
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.license_file).hint_text("A license file").desired_width(320.0));
            if ui.add_enabled(!self.license_file.trim().is_empty(), egui::Button::new("Install")).clicked() {
                let f = self.license_file.trim().to_string();
                act = Some(Box::new(move || {
                    let text = std::fs::read_to_string(&f).map_err(|e| format!("{f}: {e}"))?;
                    crate::license::install(&text).map(|l| format!("License installed: {}", crate::license::describe(&l)))
                }));
            }
        });
        ui.add_space(8.0);
        ui.label(RichText::new("Registry").strong());
        ui.horizontal(|ui| {
            ui.add(egui::TextEdit::singleline(&mut self.registry).desired_width(320.0));
            if ui.button("Use").on_hover_text("A company's own mirror, or the default again when empty").clicked() {
                crate::hosts::set_setting("registry_url", self.registry.trim());
                self.registry = registry_url();
                self.refresh(ui.ctx());
            }
        });
        if let Some(f) = act {
            self.status = Some(match f() {
                Ok(t) => (true, t),
                Err(e) => (false, e),
            });
        }
    }
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
    fn versions_and_ids() {
        assert!(newer("1.10.0", "1.9.3") && !newer("1.0.0", "1.0.0") && newer("2", "1.9"));
        assert!(valid_id("dev.sshremote.ocean-2") && !valid_id("../x") && !valid_id("a/b"));
        assert!(parse_manifest(r#"{"id":"x.y","api":9}"#).unwrap_err().contains("newer"));
        let m = parse_manifest(r#"{"id":"x.y","contributes":{"layers":[{"file":"../evil.json"},{"file":"ok.json"}]}}"#).unwrap();
        assert_eq!(m.layers.len(), 1, "files outside the folder are refused");
    }
}
