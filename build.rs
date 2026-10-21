fn main() {
    // Embed a manifest that requests elevation: ETW real-time sessions require admin.
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
        println!("cargo:rustc-link-arg-bins=/IGNORE:4099");
        // Resolve imported DLLs (pdh, wlanapi, ...) from System32 only, never from the exe's
        // folder: the exe runs elevated, often from Downloads.
        println!("cargo:rustc-link-arg-bins=/DEPENDENTLOADFLAG:0x800");
        println!("cargo:rustc-link-arg-bins=/MANIFESTUAC:level='requireAdministrator' uiAccess='false'");
    }
}
