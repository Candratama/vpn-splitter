fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        let bootstrap =
            std::fs::read_to_string("native/local-bootstrap.sh").expect("local bootstrap");
        let literal = bootstrap
            .lines()
            .map(|line| format!("{:?}\n", format!("{line}\n")))
            .collect::<String>();
        let output = std::path::PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
        std::fs::write(output.join("local-bootstrap.h"), literal).expect("bootstrap header");
        cc::Build::new()
            .include(&output)
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
