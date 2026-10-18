use serde::{Deserialize, Serialize};
use vpn_splitter_core::{Error, config::Destination, routing::ResolvedHost, session::ProfileId};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub supported: bool,
    pub split_enabled: bool,
    pub connected: bool,
    pub transitioning: bool,
}

pub struct HelperClient;

impl Drop for HelperClient {
    fn drop(&mut self) {}
}

impl HelperClient {
    pub fn connect() -> Result<Self, Error> {
        Err(Error::Transport)
    }

    pub fn observe_loss(&mut self, _callback: impl Fn() + Send + Sync + 'static) {}

    pub fn request(
        &mut self,
        _request: vpn_splitter_core::protocol::Request,
    ) -> Result<vpn_splitter_core::protocol::Reply, Error> {
        Err(Error::Transport)
    }
}

pub struct Subscription;

impl Drop for Subscription {
    fn drop(&mut self) {}
}

impl Subscription {
    pub fn new(
        _profile: &ProfileId,
        _callback: impl Fn(u32) + Send + Sync + 'static,
    ) -> Result<Self, Error> {
        Ok(Self)
    }
}

pub fn list_profiles() -> Result<Vec<Profile>, Error> {
    Ok(vec![])
}

pub fn connect(_profile: &ProfileId) -> Result<(), Error> {
    Err(Error::UnsupportedProfile)
}

pub fn disconnect(_profile: &ProfileId) -> Result<(), Error> {
    Ok(())
}

pub fn snapshot(
    _profile: &ProfileId,
) -> Result<vpn_splitter_core::routing::NetworkSnapshot, Error> {
    Err(Error::Transport)
}

pub fn helper_status() -> i32 {
    0
}

pub fn register_helper() -> Result<(), Error> {
    Err(Error::UnsupportedProfile)
}

pub fn unregister_helper() -> Result<(), Error> {
    Ok(())
}

pub fn set_login(_enabled: bool) -> Result<(), Error> {
    Ok(())
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

#[allow(dead_code)]
pub fn request_local_relaunch() {}

#[allow(dead_code)]
pub fn relaunch_local_if_requested() -> std::io::Result<()> {
    Ok(())
}
