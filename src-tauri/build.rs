#[cfg(windows)]
#[allow(clippy::permissions_set_readonly_false)]
fn make_writable(path: &std::path::Path) -> std::io::Result<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_readonly(false);
    std::fs::set_permissions(path, permissions)
}

#[cfg(not(windows))]
fn make_writable(_path: &std::path::Path) -> std::io::Result<()> {
    Ok(())
}

fn make_tree_writable(path: &std::path::Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    make_writable(path)?;
    if path.is_dir() {
        for entry in std::fs::read_dir(path)? {
            make_tree_writable(&entry?.path())?;
        }
    }
    Ok(())
}

fn main() {
    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let target_dir = out_dir
        .ancestors()
        .find(|path| path.file_name().is_some_and(|name| name == "build"))
        .and_then(|path| path.parent())
        .expect("Cargo target directory is present");
    make_tree_writable(&target_dir.join("tools"))
        .expect("generated Tauri media tools must be writable for rebuilds");
    make_tree_writable(std::path::Path::new("resources/tools"))
        .expect("bundled media tools must be writable");
    println!("cargo:rerun-if-changed=resources/tools/manifest.json");
    if std::env::var("PROFILE").as_deref() == Ok("release") {
        let suffix = if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
            ".exe"
        } else {
            ""
        };
        for tool in [
            "ffmpeg",
            "ffprobe",
            "exiftool",
            "dcraw_emu",
            "heif-convert",
            "manifest.json",
        ] {
            let name = if tool.ends_with(".json") {
                tool.to_string()
            } else {
                format!("{tool}{suffix}")
            };
            assert!(
                std::path::Path::new("resources/tools").join(name).is_file(),
                "Release builds require a complete offline media bundle. Run npm run prepare:media first."
            );
        }
    }
    tauri_build::build()
}
