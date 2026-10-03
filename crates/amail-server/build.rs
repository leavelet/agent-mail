//! Embed the web interface (`web/dist`, built with `npm run build` in
//! `web/`) into the server. Without a build, a placeholder page explains how
//! to make one, so `cargo build` never needs Node.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let dist = std::env::var_os("AMAIL_WEB_DIST")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../web/dist"));
    println!("cargo:rerun-if-env-changed=AMAIL_WEB_DIST");
    println!("cargo:rerun-if-changed={}", dist.display());
    println!("cargo:rerun-if-changed=src/placeholder.html");

    let mut files = Vec::new();
    if dist.join("index.html").is_file() {
        collect(&dist, &dist, &mut files);
    }
    files.sort();
    let built = !files.is_empty();
    let mut out = String::from("/// Files of the web interface: (path, bytes).\n");
    out.push_str("pub static ASSETS: &[(&str, &[u8])] = &[\n");
    if built {
        for (rel, abs) in &files {
            writeln!(out, "    ({rel:?}, include_bytes!({abs:?})),").unwrap();
        }
    } else {
        let p = manifest.join("src/placeholder.html");
        writeln!(
            out,
            "    (\"index.html\", include_bytes!({:?})),",
            p.display().to_string()
        )
        .unwrap();
    }
    out.push_str("];\n");
    writeln!(
        out,
        "/// Whether the real interface is embedded.\npub const BUILT: bool = {built};"
    )
    .unwrap();
    let dest = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("web_assets.rs");
    std::fs::write(dest, out).unwrap();
}

fn collect(root: &Path, dir: &Path, out: &mut Vec<(String, String)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            println!("cargo:rerun-if-changed={}", p.display());
            collect(root, &p, out);
        } else if p.is_file() {
            let rel = p
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            // Source maps stay out of the binary.
            if rel.ends_with(".map") {
                continue;
            }
            out.push((rel, p.canonicalize().unwrap().display().to_string()));
        }
    }
}
