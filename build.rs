fn main() {
    println!("cargo:rerun-if-changed=assets/app_icon.ico");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut resource = winresource::WindowsResource::new();
    resource
        .set_icon("assets/app_icon.ico")
        .set("ProductName", "LEXIS - Word Atlas")
        .set("FileDescription", "LEXIS - Word Atlas")
        .set("OriginalFilename", "word_displayer.exe");
    resource
        .compile()
        .expect("failed to embed the Windows application icon");
}
