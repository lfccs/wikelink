//! Embeds the executable's icon (the site's mark) and version details on Windows.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/wikelist.ico");
    #[cfg(windows)]
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/wikelist.ico");
        res.set("ProductName", "Wikelink");
        res.set("FileDescription", "Wikelink");
        res.set("OriginalFilename", "wikelink.exe");
        res.set("InternalName", "wikelink");
        // Without the icon the program still works: say why and go on.
        if let Err(e) = res.compile() {
            println!("cargo:warning=executable icon not embedded: {e}");
        }
    }
}
