fn main() {
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
