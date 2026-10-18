use crate::{Error, config::Config, session::ProfileId};
use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkInterface {
    pub index: u32,
    pub address: Ipv4Addr,
    pub gateway: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    pub destination: Ipv4Addr,
    pub prefix: u8,
    pub interface_id: u32,
    pub gateway: Option<Ipv4Addr>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSnapshot {
    pub profile_id: ProfileId,
    pub profile_exists: bool,
    pub supported: bool,
    pub split_enabled: bool,
    pub vpn: NetworkInterface,
    pub physical: NetworkInterface,
    pub default_interface_id: u32,
    pub vpn_endpoint: Ipv4Addr,
    pub endpoint_interface_id: u32,
    pub dns_usable: bool,
    pub existing_routes: Vec<Route>,
}

#[derive(Clone, Debug)]
pub struct ResolvedHost {
    pub hostname: String,
    pub ipv4: Vec<Ipv4Addr>,
    pub has_ipv6: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PlanWarning {
    Ipv6MayBypass { hostname: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoutePlan {
    pub profile_id: ProfileId,
    pub vpn_interface: NetworkInterface,
    pub routes: Vec<Route>,
    pub warnings: Vec<PlanWarning>,
}

fn invalid() -> Error {
    Error::InvalidInput("Invalid VPN route".into())
}
fn mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix)
    }
}
fn contains(network: Ipv4Addr, prefix: u8, ip: Ipv4Addr) -> bool {
    prefix <= 32 && (u32::from(network) & mask(prefix)) == (u32::from(ip) & mask(prefix))
}
fn usable(ip: Ipv4Addr) -> bool {
    !matches!(ip.octets()[0], 0 | 127 | 224..=255) && !ip.is_link_local()
}
impl NetworkInterface {
    pub fn validate(&self) -> Result<(), Error> {
        if self.index == 0 || !usable(self.address) || self.gateway.is_some_and(|ip| !usable(ip)) {
            return Err(invalid());
        }
        Ok(())
    }
}
impl Route {
    pub fn validate(&self) -> Result<(), Error> {
        if !(2..=32).contains(&self.prefix) || self.interface_id == 0 {
            return Err(invalid());
        }
        let start = u32::from(self.destination);
        let end = start | !mask(self.prefix);
        if start & mask(self.prefix) != start
            || !usable(self.destination)
            || !usable(Ipv4Addr::from(end))
            || contains(self.destination, self.prefix, Ipv4Addr::new(127, 0, 0, 1))
            || contains(self.destination, self.prefix, Ipv4Addr::new(169, 254, 0, 1))
            || self.gateway.is_some_and(|ip| !usable(ip))
        {
            return Err(invalid());
        }
        Ok(())
    }
}
impl RoutePlan {
    pub fn validate(&self) -> Result<(), Error> {
        self.vpn_interface.validate()?;
        if self.routes.len() > crate::config::MAX_DESTINATIONS
            || self.warnings.len() > crate::config::MAX_DESTINATIONS
        {
            return Err(invalid());
        }
        let mut seen = std::collections::HashSet::new();
        for route in &self.routes {
            route.validate()?;
            if route.interface_id != self.vpn_interface.index
                || route.gateway != self.vpn_interface.gateway
                || !seen.insert(route)
            {
                return Err(invalid());
            }
        }
        Ok(())
    }
}
pub fn build_plan(
    config: &Config,
    network: &NetworkSnapshot,
    resolved: &[ResolvedHost],
) -> Result<RoutePlan, Error> {
    use crate::config::Destination;
    let config = config.normalized()?;
    if config.profile_id.as_deref() != Some(network.profile_id.as_str())
        || !network.profile_exists
        || !network.supported
        || !network.split_enabled
    {
        return Err(Error::UnsupportedProfile);
    }
    network.vpn.validate()?;
    network.physical.validate()?;
    if network.vpn.index == network.physical.index
        || network.default_interface_id != network.physical.index
        || network.endpoint_interface_id != network.physical.index
        || network
            .existing_routes
            .iter()
            .any(|route| route.prefix <= 1 && route.interface_id != network.physical.index)
    {
        return Err(Error::Conflict);
    }
    if !network.dns_usable {
        return Err(Error::Dns);
    }
    let mut plan = RoutePlan {
        profile_id: network.profile_id.clone(),
        vpn_interface: network.vpn,
        routes: vec![],
        warnings: vec![],
    };
    let mut seen = std::collections::HashSet::new();
    for destination in &config.destinations {
        let addresses = match destination {
            Destination::Ipv4(ip) => vec![(*ip, 32)],
            Destination::Cidr { network, prefix } => vec![(*network, *prefix)],
            Destination::Hostname(hostname) => {
                let host = resolved
                    .iter()
                    .find(|host| &host.hostname == hostname)
                    .ok_or(Error::Dns)?;
                if host.ipv4.is_empty() {
                    return Err(Error::Dns);
                }
                if host.has_ipv6 {
                    plan.warnings.push(PlanWarning::Ipv6MayBypass {
                        hostname: hostname.clone(),
                    });
                }
                host.ipv4.iter().map(|ip| (*ip, 32)).collect()
            }
        };
        for (destination, prefix) in addresses {
            let route = Route {
                destination,
                prefix,
                interface_id: network.vpn.index,
                gateway: network.vpn.gateway,
            };
            route.validate()?;
            for protected in [
                Some(network.physical.address),
                network.physical.gateway,
                Some(network.vpn_endpoint),
            ]
            .into_iter()
            .flatten()
            {
                if contains(destination, prefix, protected) {
                    return Err(Error::Conflict);
                }
            }
            for existing in &network.existing_routes {
                if existing.prefix > 32 {
                    return Err(invalid());
                }
                if existing.interface_id == network.physical.index
                    && existing.gateway.is_none()
                    && (contains(existing.destination, existing.prefix, destination)
                        || contains(destination, prefix, existing.destination))
                {
                    return Err(Error::Conflict);
                }
                if existing.prefix >= prefix
                    && contains(destination, prefix, existing.destination)
                    && (existing.interface_id != route.interface_id
                        || existing.gateway != route.gateway)
                {
                    return Err(Error::Conflict);
                }
            }
            if seen.insert(route.clone()) {
                plan.routes.push(route);
            }
            if plan.routes.len() > crate::config::MAX_DESTINATIONS {
                return Err(invalid());
            }
        }
    }
    plan.validate()?;
    Ok(plan)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::config::{Destination, import_rules};

    pub fn snapshot() -> NetworkSnapshot {
        NetworkSnapshot {
            profile_id: ProfileId::try_from("Example VPN".to_string()).unwrap(),
            profile_exists: true,
            supported: true,
            split_enabled: true,
            vpn: NetworkInterface {
                index: 9,
                address: Ipv4Addr::new(203, 0, 113, 100),
                gateway: None,
            },
            physical: NetworkInterface {
                index: 4,
                address: Ipv4Addr::new(192, 0, 2, 30),
                gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
            },
            default_interface_id: 4,
            vpn_endpoint: Ipv4Addr::new(203, 0, 113, 200),
            endpoint_interface_id: 4,
            dns_usable: true,
            existing_routes: vec![],
        }
    }
    pub fn config(rules: &str) -> Config {
        Config {
            profile_id: Some("Example VPN".into()),
            destinations: import_rules(rules).unwrap(),
            ..Config::default()
        }
    }
    pub fn plan() -> RoutePlan {
        RoutePlan {
            profile_id: snapshot().profile_id,
            vpn_interface: snapshot().vpn,
            routes: vec![Route {
                destination: Ipv4Addr::new(198, 51, 100, 7),
                prefix: 32,
                interface_id: 9,
                gateway: None,
            }],
            warnings: vec![],
        }
    }

    #[test]
    fn rejects_vpn_catch_all_routes_despite_physical_default_and_endpoint() {
        for destination in ["0.0.0.0", "128.0.0.0"] {
            let mut network = snapshot();
            network.existing_routes.push(Route {
                destination: destination.parse().unwrap(),
                prefix: 1,
                interface_id: network.vpn.index,
                gateway: None,
            });
            assert!(matches!(
                build_plan(&config("198.51.100.7"), &network, &[]),
                Err(Error::Conflict)
            ));
        }
    }

    #[test]
    fn refuses_destinations_inside_the_physical_connected_subnet() {
        let mut s = snapshot();
        s.existing_routes.push(Route {
            destination: Ipv4Addr::new(192, 0, 2, 0),
            prefix: 24,
            interface_id: 4,
            gateway: None,
        });
        assert!(matches!(
            build_plan(&config("192.0.2.99"), &s, &[]),
            Err(Error::Conflict)
        ));
    }

    #[test]
    fn plans_only_explicit_vpn_destinations_and_deduplicates_dns_answers() {
        let c = config("198.51.100.129/25\ngit.office.example\n198.51.100.7");
        let resolved = vec![ResolvedHost {
            hostname: "git.office.example".into(),
            ipv4: vec![Ipv4Addr::new(198, 51, 100, 7); 2],
            has_ipv6: true,
        }];
        let plan = build_plan(&c, &snapshot(), &resolved).unwrap();
        assert_eq!(
            plan.routes,
            vec![
                Route {
                    destination: Ipv4Addr::new(198, 51, 100, 128),
                    prefix: 25,
                    interface_id: 9,
                    gateway: None
                },
                Route {
                    destination: Ipv4Addr::new(198, 51, 100, 7),
                    prefix: 32,
                    interface_id: 9,
                    gateway: None
                }
            ]
        );
        assert_eq!(
            plan.warnings,
            vec![PlanWarning::Ipv6MayBypass {
                hostname: "git.office.example".into()
            }]
        );
    }

    #[test]
    fn rejects_full_tunnel_missing_profile_and_changed_public_or_endpoint_paths() {
        for case in 0..5 {
            let mut s = snapshot();
            match case {
                0 => s.split_enabled = false,
                1 => s.profile_exists = false,
                2 => s.supported = false,
                3 => s.default_interface_id = 9,
                _ => s.endpoint_interface_id = 9,
            }
            assert!(build_plan(&config("198.51.100.7"), &s, &[]).is_err());
        }
        let mut c = config("198.51.100.7");
        c.profile_id = None;
        assert!(build_plan(&c, &snapshot(), &[]).is_err());
    }

    #[test]
    fn refuses_unresolved_ipv6_only_and_expanded_route_overflow() {
        let c = config("git.office.example");
        assert!(matches!(build_plan(&c, &snapshot(), &[]), Err(Error::Dns)));
        let mut r = ResolvedHost {
            hostname: "git.office.example".into(),
            ipv4: vec![],
            has_ipv6: true,
        };
        assert!(build_plan(&c, &snapshot(), &[r.clone()]).is_err());
        r.has_ipv6 = false;
        r.ipv4 = (1..=4097).map(|n| Ipv4Addr::from(0x0a000000 + n)).collect();
        assert!(build_plan(&c, &snapshot(), &[r]).is_err());
    }

    #[test]
    fn rejects_control_path_overlap_and_foreign_more_specific_routes() {
        for input in ["192.0.2.0/24", "203.0.113.200", "127.0.0.1", "224.1.2.3"] {
            assert!(
                build_plan(&config(input), &snapshot(), &[]).is_err(),
                "{input}"
            );
        }
        let mut s = snapshot();
        s.existing_routes.push(Route {
            destination: Ipv4Addr::new(198, 51, 100, 7),
            prefix: 32,
            interface_id: 4,
            gateway: Some(Ipv4Addr::new(192, 0, 2, 1)),
        });
        assert!(matches!(
            build_plan(&config("198.51.100.0/24"), &s, &[]),
            Err(Error::Conflict)
        ));
        s.existing_routes[0].interface_id = 9;
        s.existing_routes[0].gateway = None;
        assert!(build_plan(&config("198.51.100.0/24"), &s, &[]).is_ok());
    }

    #[test]
    fn validates_typed_route_plans_at_the_wire_boundary() {
        assert!(plan().validate().is_ok());
        for prefix in [0, 1, 33] {
            let mut p = plan();
            p.routes[0].prefix = prefix;
            assert!(p.validate().is_err());
        }
        let mut p = plan();
        p.routes[0].interface_id = 4;
        assert!(p.validate().is_err());
        let mut c = config("198.51.100.7");
        c.destinations = vec![Destination::Cidr {
            network: Ipv4Addr::UNSPECIFIED,
            prefix: 0,
        }];
        assert!(build_plan(&c, &snapshot(), &[]).is_err());
    }
}
