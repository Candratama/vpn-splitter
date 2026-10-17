fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        cc::Build::new()
            .file("native/network.m")
            .file("native/ipc.m")
            .flag("-fobjc-arc")
            .flag("-fblocks")
            .flag("-mmacosx-version-min=14.0")
            .compile("vpn_splitter_native");
        for framework in [
            "Foundation",
            "SystemConfiguration",
            "Security",
            "ServiceManagement",
            "AppKit",
            "IOKit",
        ] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        println!("cargo:rerun-if-changed=native");
    }
}
