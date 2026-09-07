fn main() {
    tauri_build::build();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Tauri normally links the Common Controls v6 manifest only into app
        // binaries. Its mock-window integration tests need that manifest too.
        let out = std::env::var("OUT_DIR").expect("OUT_DIR");
        println!("cargo:rustc-link-arg-tests={out}/resource.lib");
    }
}
