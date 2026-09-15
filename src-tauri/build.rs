fn main() {
    assert!(
        std::env::var("PROFILE").as_deref() != Ok("release") || !tauri_build::is_dev(),
        "Standalone release builds must embed the frontend. Run npm run package -- --no-bundle -- --locked, or enable tauri/custom-protocol after npm run build."
    );
    tauri_build::build();
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/notch.m")
            .flag("-fobjc-arc")
            .flag("-mmacosx-version-min=12.0")
            .flag("-Wno-unused-parameter")
            .compile("transcribe_notch");
        for framework in ["AppKit", "QuartzCore", "ApplicationServices"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rerun-if-changed=native/notch.m");
        println!("cargo:rerun-if-changed=native/notch.h");
    }
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        // Tauri normally links the Common Controls v6 manifest only into app
        // binaries. Its mock-window integration tests need that manifest too.
        let out = std::env::var("OUT_DIR").expect("OUT_DIR");
        println!("cargo:rustc-link-arg-tests={out}/resource.lib");
    }
}
