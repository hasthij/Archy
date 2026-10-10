// Embeds Archy's icon in archy.exe (Explorer, taskbar, Start menu).
fn main() {
    println!("cargo:rerun-if-changed=../../assets/archy.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("../../assets/archy.ico");
        res.set("FileDescription", "Archy");
        res.set("ProductName", "Archy");
        if let Err(e) = res.compile() {
            println!("cargo:warning=icon not embedded: {e}");
        }
    }
}
