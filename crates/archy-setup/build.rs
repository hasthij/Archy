//! Embeds the freshly built archy.exe and archy-ui.exe into the installer.
//! Build them first: `cargo build --release -p archy -p archy-ui`, then this crate.

use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    // OUT_DIR = target/<profile>/build/archy-setup-*/out
    let profile_dir = out.ancestors().nth(3).expect("target dir").to_path_buf();
    for name in ["archy.exe", "archy-ui.exe"] {
        let src = profile_dir.join(name);
        println!("cargo:rerun-if-changed={}", src.display());
        if !src.exists() {
            panic!(
                "{} not found: build archy and archy-ui first",
                src.display()
            );
        }
        std::fs::copy(&src, out.join(name)).expect("copy payload");
    }
    println!("cargo:rerun-if-changed=../../assets/archy.ico");
    let mut res = winresource::WindowsResource::new();
    res.set_icon("../../assets/archy.ico");
    res.set("FileDescription", "Archy Setup");
    res.set("ProductName", "Archy");
    if let Err(e) = res.compile() {
        println!("cargo:warning=icon not embedded: {e}");
    }
}
