//! Compiles the Slint UI with the Fluent style and embeds the icon and version info.

fn main() {
    let config = slint_build::CompilerConfiguration::new().with_style("fluent".into());
    slint_build::compile_with_config("ui/app.slint", config).expect("Slint UI failed to compile");

    println!("cargo:rerun-if-changed=assets/randomovie.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut resource = winresource::WindowsResource::new();
        resource
            .set_icon("assets/randomovie.ico")
            .set("ProductName", "RandoMovie")
            .set("FileDescription", "RandoMovie")
            .set("OriginalFilename", "RandoMovie.exe")
            .set("InternalName", "RandoMovie")
            .set("LegalCopyright", "Personal, non-commercial use");
        resource
            .compile()
            .expect("Windows resources (icon, version info) failed to compile");
    }
}
