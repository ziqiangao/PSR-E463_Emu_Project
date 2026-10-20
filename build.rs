use std::{env, fs, path::PathBuf};

fn main() {
    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let config_path = manifest.join("ui/controls.json");
    let config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    let artwork = manifest
        .join("ui")
        .join(config["artwork"].as_str().unwrap());
    println!("cargo:rerun-if-changed={}", config_path.display());
    println!("cargo:rerun-if-changed={}", artwork.display());
    println!("cargo:rustc-env=PSR_UI_CONFIG={}", config_path.display());
    println!("cargo:rustc-env=PSR_UI_ARTWORK={}", artwork.display());
}
