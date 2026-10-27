// Windows: embed the icon and version info into the exe (shown by Explorer and the taskbar)
fn main() {
    println!("cargo:rerun-if-changed=assets/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/app.ico")
            .set("ProductName", "QCCOM")
            .set("FileDescription", "QCCOM - Serial / TCP / UDP / MQTT debug tool")
            .set("OriginalFilename", "qccom.exe");
        if let Err(e) = res.compile() {
            println!("cargo:warning=failed to embed the Windows icon: {e}");
        }
    }
}
