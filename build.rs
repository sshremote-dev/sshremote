//! Embeds the logo as the Windows executable icon when a resource compiler is available.

fn main() {
    println!("cargo:rerun-if-changed=assets/sshremote.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("gnu")
    {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("sshremote.ico");
    let rc = out.join("sshremote.rc");
    std::fs::write(&rc, format!("1 ICON \"{}\"\n", ico.display().to_string().replace('\\', "/"))).unwrap();
    let obj = out.join("sshremote-icon.o");
    let windres = std::env::var("WINDRES").unwrap_or_else(|_| "windres".into());
    match std::process::Command::new(&windres).arg("-O").arg("coff").arg("-i").arg(&rc).arg("-o").arg(&obj).status() {
        Ok(s) if s.success() => println!("cargo:rustc-link-arg-bins={}", obj.display()),
        _ => println!("cargo:warning=windres not found; sshremote.exe is built without an icon"),
    }
}
