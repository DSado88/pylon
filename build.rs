fn main() {
    println!("cargo:rerun-if-changed=src/gpu/shaders/cell.metal");

    let out_dir = std::env::var("OUT_DIR").unwrap_or_else(|_| "target".to_string());
    let metallib_path = format!("{out_dir}/cell.metallib");

    // Compile .metal -> .air -> .metallib
    let compiled = (|| -> Option<()> {
        let s = std::process::Command::new("xcrun")
            .args(["-sdk", "macosx", "metal", "-c", "src/gpu/shaders/cell.metal", "-o"])
            .arg(format!("{out_dir}/cell.air"))
            .status()
            .ok()?;
        if !s.success() { return None; }

        let s = std::process::Command::new("xcrun")
            .args(["metallib"])
            .arg(format!("{out_dir}/cell.air"))
            .arg("-o")
            .arg(&metallib_path)
            .status()
            .ok()?;
        if !s.success() { return None; }
        Some(())
    })();

    if compiled.is_none() {
        // Create an empty sentinel file so include_bytes! succeeds.
        // The runtime will detect the empty file and fall back to source compilation.
        std::fs::write(&metallib_path, []).ok();
        println!("cargo:warning=Metal offline compilation unavailable; will compile shaders at runtime");
    }
}
