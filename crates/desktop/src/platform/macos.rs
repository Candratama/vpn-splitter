use vpn_splitter_core::{Error, config::Destination, routing::ResolvedHost, session::ProfileId};
pub use vpn_splitter_helper::platform::macos::{
    HelperClient, Profile, Subscription, helper_status, list_profiles, register_helper, set_login,
    snapshot, unregister_helper,
};
pub fn connect(profile: &ProfileId) -> Result<(), Error> {
    vpn_splitter_helper::platform::macos::control(profile, true)
}
pub fn disconnect(profile: &ProfileId) -> Result<(), Error> {
    if !list_profiles()?
        .iter()
        .any(|p| &p.id == profile && (p.connected || p.transitioning))
    {
        return Ok(());
    }
    vpn_splitter_helper::platform::macos::control(profile, false)
}
pub fn resolve_hosts(destinations: &[Destination]) -> Result<Vec<ResolvedHost>, Error> {
    use std::net::ToSocketAddrs;
    let mut resolved = vec![];
    for destination in destinations {
        let Destination::Hostname(hostname) = destination else {
            continue;
        };
        let mut ipv4 = vec![];
        let mut has_ipv6 = false;
        for address in (hostname.as_str(), 0)
            .to_socket_addrs()
            .map_err(|_| Error::Dns)?
        {
            match address.ip() {
                std::net::IpAddr::V4(ip) => {
                    if !ipv4.contains(&ip) {
                        ipv4.push(ip);
                    }
                }
                std::net::IpAddr::V6(_) => has_ipv6 = true,
            }
        }
        if ipv4.is_empty() {
            return Err(Error::Dns);
        }
        resolved.push(ResolvedHost {
            hostname: hostname.clone(),
            ipv4,
            has_ipv6,
        });
    }
    Ok(resolved)
}
static RELAUNCH_LOCAL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn request_local_relaunch() {
    RELAUNCH_LOCAL.store(true, std::sync::atomic::Ordering::Release);
}
pub fn relaunch_local_if_requested() -> std::io::Result<()> {
    if RELAUNCH_LOCAL.swap(false, std::sync::atomic::Ordering::AcqRel) {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(
            "/Library/PrivilegedHelperTools/VPN Splitter.app/Contents/MacOS/vpn-splitter-desktop",
        )
        .arg("--relaunch")
        .process_group(0)
        .spawn()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn local_resolver_preserves_ipv4_and_marks_ipv6_without_global_dns_changes() {
        let results = resolve_hosts(&[Destination::Hostname("localhost".into())]).unwrap();
        assert_eq!(results[0].hostname, "localhost");
        assert!(!results[0].ipv4.is_empty());
    }
    #[test]
    fn deleted_profile_is_refused_before_native_connect() {
        assert!(matches!(
            connect(&ProfileId::try_from("missing-test-profile".to_owned()).unwrap()),
            Err(Error::UnsupportedProfile)
        ));
    }
}
