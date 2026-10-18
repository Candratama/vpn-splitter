fn main() {
    #[cfg(target_os = "macos")]
    let result = if std::env::args().nth(1).as_deref() == Some("--cleanup") {
        vpn_splitter_helper::platform::macos::cleanup_for_uninstall()
    } else {
        vpn_splitter_helper::platform::macos::serve()
    };
    #[cfg(not(target_os = "macos"))]
    let result: Result<(), vpn_splitter_core::Error> =
        Err(vpn_splitter_core::Error::UnsupportedProfile);
    if let Err(error) = result {
        eprintln!("VPN Splitter helper: {}", error.code());
        std::process::exit(1);
    }
}
