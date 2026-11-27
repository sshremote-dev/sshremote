//! Deflates the language tables, and embeds the logo as the Windows executable icon, and its
//! manifest, when a resource compiler is available: MinGW's windres for the GNU toolchain, the
//! Windows SDK's rc for MSVC.

use std::path::{Path, PathBuf};

/// Each language's table deflated, for the program to carry (it inflates only the one it speaks).
fn lang_tables() {
    use std::io::Write;
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("lang");
    println!("cargo:rerun-if-changed={}", dir.display());
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("lang");
    std::fs::create_dir_all(&out).unwrap();
    for e in std::fs::read_dir(&dir).unwrap().flatten() {
        let p = e.path();
        if p.extension().is_none_or(|x| x != "json") {
            continue;
        }
        println!("cargo:rerun-if-changed={}", p.display());
        let mut z = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        z.write_all(&std::fs::read(&p).unwrap()).unwrap();
        std::fs::write(out.join(format!("{}.z", p.file_name().unwrap().to_string_lossy())), z.finish().unwrap()).unwrap();
    }
}

fn main() {
    lang_tables();
    println!("cargo:rerun-if-changed=assets/sshremote.ico");
    println!("cargo:rerun-if-changed=assets/sshremote.manifest");
    println!("cargo:rerun-if-env-changed=SSHREMOTE_REQUIRE_RESOURCES");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let msvc = std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("sshremote.ico");
    let rc = out.join("sshremote.rc");
    let manifest = ico.with_file_name("sshremote.manifest");
    let path = |p: &Path| p.display().to_string().replace('\\', "/");
    // The icon, and the manifest that keeps a double click from opening a console window.
    std::fs::write(&rc, format!("1 ICON \"{}\"\n1 24 \"{}\"\n", path(&ico), path(&manifest))).unwrap();
    let compiled = if msvc { out.join("sshremote.res") } else { out.join("sshremote-icon.o") };
    let run = |cmd: &mut std::process::Command| cmd.status().is_ok_and(|s| s.success());
    let built = if msvc {
        sdk_rc().iter().any(|rc_exe| run(std::process::Command::new(rc_exe).arg("/nologo").arg("/fo").arg(&compiled).arg(&rc)))
    } else {
        // WINDRES when set; else the native one, or MinGW's when building for Windows elsewhere.
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
        let candidates = std::env::var("WINDRES").map_or_else(|_| vec!["windres".to_string(), format!("{arch}-w64-mingw32-windres")], |w| vec![w]);
        candidates.iter().any(|w| run(std::process::Command::new(w).arg("-O").arg("coff").arg("-i").arg(&rc).arg("-o").arg(&compiled)))
    };
    if built {
        println!("cargo:rustc-link-arg-bins={}", compiled.display());
    } else if std::env::var_os("SSHREMOTE_REQUIRE_RESOURCES").is_some() {
        // A release build: the program is not shipped without them.
        panic!("no resource compiler: sshremote.exe would be built without its icon and manifest");
    } else {
        println!("cargo:warning=no resource compiler; sshremote.exe is built without its icon and manifest");
    }
}

/// The Windows SDK's rc.exe: $RC, the one on PATH, then the newest of each SDK installed.
fn sdk_rc() -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::env::var_os("RC").map(PathBuf::from).into_iter().collect();
    found.push("rc.exe".into());
    let base = std::env::var_os("ProgramFiles(x86)").map_or_else(|| PathBuf::from(r"C:\Program Files (x86)"), PathBuf::from);
    let mut sdks: Vec<PathBuf> = std::fs::read_dir(base.join(r"Windows Kits\10\bin"))
        .map(|d| d.flatten().map(|e| e.path()).filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("10."))).collect())
        .unwrap_or_default();
    sdks.sort();
    for sdk in sdks.iter().rev() {
        found.extend(["arm64", "x64", "x86"].iter().map(|host| sdk.join(host).join("rc.exe")).filter(|p| p.exists()));
    }
    found
}
