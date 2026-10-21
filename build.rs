fn main() {
    // Embed the application icon and version info in the Windows executable.
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Rustyrobo");
        res.set("FileDescription", "Rustyrobo — native MongoDB GUI");
        if let Err(e) = res.compile() {
            println!("cargo:warning=cannot embed the Windows icon: {e}");
        }
    }
    println!("cargo:rerun-if-changed=assets/icon.ico");
}
