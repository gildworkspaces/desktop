use std::path::{Path, PathBuf};
use std::process::Command;

/// The add-ons that ship with gild, each its own crate under `addons/<name>`,
/// built to WebAssembly here so the app embeds the same module a third-party
/// add-on would ship.
const ADDONS: [&str; 1] = ["markdown"];
const TARGET: &str = "wasm32-unknown-unknown";

/// A tool from rustup's toolchain when there is one (it carries the
/// WebAssembly target; a system Rust may not), else the one building us.
fn rustup_tool(name: &str) -> Option<PathBuf> {
    let out = Command::new("rustup").args(["which", name]).output().ok()?;
    out.status.success().then(|| PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string()))
}

fn build_addon(name: &str, out_dir: &Path) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("addons").join(name);
    for p in ["src", "Cargo.toml", "Cargo.lock", "gild-addon.json"] {
        println!("cargo:rerun-if-changed={}", dir.join(p).display());
    }
    let target_dir = out_dir.join("addons");
    let (cargo, rustc) = match (rustup_tool("cargo"), rustup_tool("rustc")) {
        (Some(c), Some(r)) => (c, Some(r)),
        _ => (PathBuf::from(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into())), None),
    };
    let mut cmd = Command::new(&cargo);
    cmd.args(["build", "--release", "--locked", "--target", TARGET, "--manifest-path"])
        .arg(dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(&target_dir)
        // The app's build settings are not the add-on's.
        .env_remove("RUSTFLAGS")
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        // Under clippy, cargo runs rustc through clippy-driver: the add-on is
        // built the same either way.
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .env_remove("CLIPPY_ARGS")
        .env_remove("CLIPPY_CONF_DIR");
    match &rustc {
        Some(r) => cmd.env("RUSTC", r),
        None => cmd.env_remove("RUSTC"),
    };
    let status = cmd.status().unwrap_or_else(|e| panic!("building the {name} add-on: couldn't run {}: {e}", cargo.display()));
    if !status.success() {
        panic!("building the {name} add-on failed. It needs the {TARGET} target: rustup target add {TARGET}");
    }
    let wasm = target_dir.join(TARGET).join("release").join(format!("gild_addon_{name}.wasm"));
    println!("cargo:rustc-env=GILD_ADDON_{}_WASM={}", name.to_uppercase(), wasm.display());
}

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo sets OUT_DIR"));
    for name in ADDONS {
        build_addon(name, &out_dir);
    }
    tauri_build::build()
}
