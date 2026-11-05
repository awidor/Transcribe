fn main() {
    println!("cargo:rerun-if-changed=windows/transcribe.rc");
    println!("cargo:rerun-if-changed=windows/transcribe.manifest");
    // The icon and manifest need the Windows resource compiler.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") && cfg!(windows) {
        embed_resource::compile("windows/transcribe.rc", embed_resource::NONE)
            .manifest_required()
            .unwrap();
    }
}
