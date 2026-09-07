fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/macos.m")
            .file("native/hotkey.m")
            .flag("-fobjc-arc")
            .compile("transcribe_native");
        for framework in ["AppKit", "ApplicationServices", "Carbon"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rerun-if-changed=native/macos.m");
        println!("cargo:rerun-if-changed=native/hotkey.m");
    }
}
