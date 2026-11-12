//! Embeds the logo as the Windows executable icon when a resource compiler is available.

fn main() {
    println!("cargo:rerun-if-changed=assets/sshremote.ico");
    println!("cargo:rerun-if-changed=assets/sshremote.manifest");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("sshremote.ico");
    let rc = out.join("sshremote.rc");
    let manifest = ico.with_file_name("sshremote.manifest");
    let path = |p: &std::path::Path| p.display().to_string().replace('\\', "/");
    // The icon, and the manifest that keeps a double click from opening a console window.
    std::fs::write(&rc, format!("1 ICON \"{}\"\n1 24 \"{}\"\n", path(&ico), path(&manifest))).unwrap();
    let obj = out.join("sshremote-icon.o");
    // WINDRES when set; else the native one, or MinGW's when building for Windows elsewhere.
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let candidates = std::env::var("WINDRES").map_or_else(|_| vec!["windres".to_string(), format!("{arch}-w64-mingw32-windres")], |w| vec![w]);
    let built = candidates.iter().any(|w| {
        std::process::Command::new(w).arg("-O").arg("coff").arg("-i").arg(&rc).arg("-o").arg(&obj).status().is_ok_and(|s| s.success())
    });
    if built {
        println!("cargo:rustc-link-arg-bins={}", obj.display());
    } else {
        println!("cargo:warning=windres not found; sshremote.exe is built without its icon and manifest");
    }
}
