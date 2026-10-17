fn main() {
    #[cfg(target_os = "macos")]
    let result = vpn_splitter_helper::platform::macos::serve();
    #[cfg(not(target_os = "macos"))]
    let result: Result<(), vpn_splitter_core::Error> =
        Err(vpn_splitter_core::Error::UnsupportedProfile);
    if let Err(error) = result {
        eprintln!("VPN Splitter helper: {}", error.code());
        std::process::exit(1);
    }
}
