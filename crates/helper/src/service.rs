//! Policy boundary for an authenticated native connection. Transport authentication
//! must supply PeerIdentity; none of these fields are accepted from JSON.
use crate::journal::{Journal, RouteBackend};
use vpn_splitter_core::{
    Error,
    protocol::{Reply, Request},
    routing::{NetworkInterface, NetworkSnapshot},
    session::ProfileId,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    pub(crate) uid: u32,
    pub(crate) connection: u64,
    pub(crate) verified_requirement: String,
}
pub fn authorize_peer(
    peer: &PeerIdentity,
    console_uid: u32,
    requirement: &str,
) -> Result<(), Error> {
    if peer.uid == console_uid
        && console_uid != 0
        && peer.verified_requirement == requirement
        && !requirement.is_empty()
    {
        Ok(())
    } else {
        Err(Error::Permission)
    }
}
struct Binding {
    peer: PeerIdentity,
    session: vpn_splitter_core::session::SessionId,
    profile: ProfileId,
    vpn: NetworkInterface,
    sequence: u64,
    open: bool,
}
pub struct Service<B> {
    journal: Journal,
    backend: B,
    console_uid: u32,
    requirement: String,
    binding: Option<Binding>,
}
fn fault(error: &Error) -> vpn_splitter_core::protocol::FaultCode {
    use vpn_splitter_core::protocol::FaultCode;
    match error {
        Error::InvalidInput(_) => FaultCode::InvalidInput,
        Error::Permission => FaultCode::Permission,
        Error::Timeout => FaultCode::Timeout,
        Error::Conflict => FaultCode::Conflict,
        Error::UnsupportedProfile => FaultCode::UnsupportedProfile,
        Error::Dns => FaultCode::Dns,
        Error::Transport => FaultCode::Transport,
        Error::Cleanup => FaultCode::Cleanup,
        Error::Io(_) => FaultCode::Io,
    }
}
fn attest(
    profile: &ProfileId,
    vpn: NetworkInterface,
    plan: Option<&vpn_splitter_core::routing::RoutePlan>,
    snapshot: &mut impl FnMut(&ProfileId, NetworkInterface) -> Result<NetworkSnapshot, Error>,
) -> Result<NetworkSnapshot, Error> {
    use vpn_splitter_core::{
        config::{Config, Destination},
        routing::build_plan,
    };
    let network = snapshot(profile, vpn)?;
    if network.vpn != vpn {
        return Err(Error::Conflict);
    }
    let config = Config {
        profile_id: Some(profile.as_str().to_owned()),
        destinations: plan
            .map(|p| {
                p.routes
                    .iter()
                    .map(|r| Destination::Cidr {
                        network: r.destination,
                        prefix: r.prefix,
                    })
                    .collect()
            })
            .unwrap_or_default(),
        ..Config::default()
    };
    build_plan(&config, &network, &[])?;
    Ok(network)
}
impl<B: RouteBackend> Service<B> {
    pub fn recover(
        mut journal: Journal,
        mut backend: B,
        console_uid: u32,
        requirement: String,
    ) -> Result<Self, Error> {
        if console_uid == 0 || requirement.is_empty() {
            return Err(Error::Permission);
        }
        // Keep authenticated Status/Clear available even when startup cleanup
        // cannot finish. Journal guards continue to block new route ownership.
        let _ = crate::journal::reconcile(&mut journal, &mut backend);
        Ok(Self {
            journal,
            backend,
            console_uid,
            requirement,
            binding: None,
        })
    }
    pub fn request(
        &mut self,
        peer: &PeerIdentity,
        bytes: &[u8],
        snapshot: &mut impl FnMut(&ProfileId, NetworkInterface) -> Result<NetworkSnapshot, Error>,
    ) -> Result<Reply, Error> {
        use vpn_splitter_core::protocol::ReplyResult;
        authorize_peer(peer, self.console_uid, &self.requirement)?;
        let request = Request::decode(bytes)?;
        let header = request.header();
        if let Request::Begin {
            profile_id,
            vpn_interface,
            ..
        } = &request
        {
            if header.sequence != 1
                || self
                    .binding
                    .as_ref()
                    .is_some_and(|b| b.open || b.session == header.session)
            {
                return Err(Error::Conflict);
            }
            if self.journal.has_pending() || self.journal.requires_reconciliation() {
                return Err(Error::Cleanup);
            }
            attest(profile_id, *vpn_interface, None, snapshot)?;
            self.journal.begin(header.session)?;
            self.binding = Some(Binding {
                peer: peer.clone(),
                session: header.session,
                profile: profile_id.clone(),
                vpn: *vpn_interface,
                sequence: 1,
                open: true,
            });
            return Ok(Reply {
                header,
                result: ReplyResult::Begun,
            });
        }
        if self.binding.is_none() && matches!(request, Request::Clear { .. }) {
            let result = match crate::journal::reconcile(&mut self.journal, &mut self.backend) {
                Ok(report) if report.complete() => ReplyResult::Cleared,
                _ => ReplyResult::Rejected {
                    code: vpn_splitter_core::protocol::FaultCode::Cleanup,
                    previous_plan_restored: false,
                },
            };
            return Ok(Reply { header, result });
        }
        if self.binding.is_none() && matches!(request, Request::Status { .. }) {
            return Ok(Reply {
                header,
                result: ReplyResult::Status {
                    active: false,
                    cleanup_required: self.journal.requires_reconciliation(),
                },
            });
        }
        let binding = self.binding.as_mut().ok_or(Error::Permission)?;
        if &binding.peer != peer
            || binding.session != header.session
            || binding.sequence.checked_add(1) != Some(header.sequence)
        {
            return Err(Error::Permission);
        }
        binding.sequence = header.sequence;
        let result = match request {
            Request::Apply { plan, .. } => {
                if !binding.open
                    || plan.profile_id != binding.profile
                    || plan.vpn_interface != binding.vpn
                {
                    return Err(Error::Conflict);
                }
                attest(&binding.profile, binding.vpn, Some(&plan), snapshot)?;
                match crate::journal::apply(&mut self.journal, &plan, &mut self.backend) {
                    Ok(()) => ReplyResult::Applied,
                    Err(error) => {
                        let restored = if let Some(previous) = self.journal.committed_plan() {
                            attest(&binding.profile, binding.vpn, Some(previous), snapshot).is_ok()
                                && previous.routes.iter().all(|r| {
                                    self.backend.read(r).ok().flatten().as_ref() == Some(r)
                                })
                                && !self.journal.requires_reconciliation()
                        } else {
                            false
                        };
                        ReplyResult::Rejected {
                            code: fault(&error),
                            previous_plan_restored: restored,
                        }
                    }
                }
            }
            Request::Clear { .. } => {
                if !binding.open {
                    ReplyResult::Cleared
                } else {
                    match crate::journal::clear(&mut self.journal, &mut self.backend) {
                        Ok(report) if report.complete() => {
                            binding.open = false;
                            ReplyResult::Cleared
                        }
                        _ => ReplyResult::Rejected {
                            code: vpn_splitter_core::protocol::FaultCode::Cleanup,
                            previous_plan_restored: false,
                        },
                    }
                }
            }
            Request::Status { .. } => {
                let active = binding.open
                    && !self.journal.requires_reconciliation()
                    && self.journal.committed_plan().is_some_and(|plan| {
                        attest(&binding.profile, binding.vpn, Some(plan), snapshot).is_ok()
                            && plan
                                .routes
                                .iter()
                                .all(|r| self.backend.read(r).ok().flatten().as_ref() == Some(r))
                    });
                ReplyResult::Status {
                    active,
                    cleanup_required: self.journal.requires_reconciliation()
                        || (binding.open && self.journal.committed_plan().is_some() && !active),
                }
            }
            Request::Begin { .. } => unreachable!(),
        };
        Ok(Reply { header, result })
    }
    pub fn network_is_valid(
        &mut self,
        snapshot: &mut impl FnMut(&ProfileId, NetworkInterface) -> Result<NetworkSnapshot, Error>,
    ) -> bool {
        let Some(binding) = self.binding.as_ref().filter(|binding| binding.open) else {
            return true;
        };
        if self.journal.requires_reconciliation() {
            return false;
        }
        let plan = self.journal.committed_plan();
        attest(&binding.profile, binding.vpn, plan, snapshot).is_ok_and(|network| {
            plan.is_none_or(|plan| {
                plan.routes
                    .iter()
                    .all(|route| network.existing_routes.contains(route))
            })
        })
    }
    pub fn has_open_session(&self) -> bool {
        self.binding.as_ref().is_some_and(|binding| binding.open)
            || self.journal.requires_reconciliation()
    }
    pub fn invalidated(&mut self, connection: u64) -> Result<(), Error> {
        if !self
            .binding
            .as_ref()
            .is_some_and(|b| b.peer.connection == connection)
        {
            return Ok(());
        }
        self.binding = None;
        if !crate::journal::clear(&mut self.journal, &mut self.backend)?.complete() {
            return Err(Error::Cleanup);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use vpn_splitter_core::{
        protocol::{Header, ReplyResult},
        routing::{Route, RoutePlan},
        session::SessionId,
    };
    #[derive(Default)]
    struct Backend {
        routes: Vec<Route>,
        mutations: usize,
        fail_delete: bool,
    }
    impl RouteBackend for Backend {
        fn read(&mut self, r: &Route) -> Result<Option<Route>, Error> {
            Ok(self
                .routes
                .iter()
                .find(|v| v.destination == r.destination && v.prefix == r.prefix)
                .cloned())
        }
        fn add(&mut self, r: &Route) -> Result<(), Error> {
            self.mutations += 1;
            self.routes.push(r.clone());
            Ok(())
        }
        fn delete(&mut self, r: &Route) -> Result<(), Error> {
            if self.fail_delete {
                return Err(Error::Cleanup);
            }
            self.mutations += 1;
            self.routes.retain(|v| v != r);
            Ok(())
        }
    }
    fn peer() -> PeerIdentity {
        PeerIdentity {
            uid: 501,
            connection: 1,
            verified_requirement: "test signed application requirement".into(),
        }
    }
    fn snapshot(profile: &ProfileId, vpn: NetworkInterface) -> Result<NetworkSnapshot, Error> {
        Ok(NetworkSnapshot {
            profile_id: profile.clone(),
            profile_exists: true,
            supported: true,
            split_enabled: true,
            vpn,
            physical: NetworkInterface {
                index: 4,
                address: "192.0.2.30".parse().unwrap(),
                gateway: Some("192.0.2.1".parse().unwrap()),
            },
            default_interface_id: 4,
            vpn_endpoint: "203.0.113.200".parse().unwrap(),
            endpoint_interface_id: 4,
            dns_usable: true,
            existing_routes: vec![],
        })
    }
    fn request(sequence: u64, session: SessionId) -> Request {
        Request::Begin {
            header: Header {
                version: 1,
                session,
                sequence,
            },
            profile_id: ProfileId::try_from("Example VPN".to_owned()).unwrap(),
            vpn_interface: NetworkInterface {
                index: 9,
                address: "203.0.113.10".parse().unwrap(),
                gateway: None,
            },
        }
    }
    fn service() -> (tempfile::TempDir, Service<Backend>) {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let journal = Journal::open(&dir.path().join("journal.json")).unwrap();
        let s = Service::recover(
            journal,
            Backend::default(),
            501,
            peer().verified_requirement,
        )
        .unwrap();
        (dir, s)
    }
    fn rejected(result: Result<Reply, Error>) {
        assert!(result.is_err() || matches!(result.unwrap().result, ReplyResult::Rejected { .. }));
    }
    #[test]
    fn delayed_network_event_keeps_valid_routes_and_rejects_lost_paths() {
        let (_dir, mut service) = service();
        let id = SessionId::new();
        let begin = request(1, id);
        service
            .request(&peer(), &begin.encode().unwrap(), &mut snapshot)
            .unwrap();
        assert!(service.network_is_valid(&mut snapshot));
        let Request::Begin {
            profile_id,
            vpn_interface,
            ..
        } = begin
        else {
            unreachable!()
        };
        let plan = RoutePlan {
            profile_id,
            vpn_interface,
            routes: vec![Route {
                destination: "198.51.100.7".parse().unwrap(),
                prefix: 32,
                interface_id: 9,
                gateway: None,
            }],
            warnings: vec![],
        };
        service
            .request(
                &peer(),
                &Request::Apply {
                    header: Header {
                        version: 1,
                        session: id,
                        sequence: 2,
                    },
                    plan,
                }
                .encode()
                .unwrap(),
                &mut snapshot,
            )
            .unwrap();
        let routes = service.backend.routes.clone();
        assert!(service.network_is_valid(&mut |p, v| {
            let mut network = snapshot(p, v)?;
            network.existing_routes = routes.clone();
            Ok(network)
        }));
        assert!(!service.network_is_valid(&mut snapshot));
        assert!(!service.network_is_valid(&mut |p, v| {
            let mut network = snapshot(p, v)?;
            network.default_interface_id = v.index;
            Ok(network)
        }));
    }
    #[test]
    fn failed_startup_cleanup_keeps_status_and_clear_available() {
        let (_dir, mut initial) = service();
        let id = SessionId::new();
        let begin = request(1, id);
        initial
            .request(&peer(), &begin.encode().unwrap(), &mut snapshot)
            .unwrap();
        let Request::Begin {
            profile_id,
            vpn_interface,
            ..
        } = begin
        else {
            unreachable!()
        };
        let plan = RoutePlan {
            profile_id,
            vpn_interface,
            routes: vec![Route {
                destination: "198.51.100.7".parse().unwrap(),
                prefix: 32,
                interface_id: 9,
                gateway: None,
            }],
            warnings: vec![],
        };
        initial
            .request(
                &peer(),
                &Request::Apply {
                    header: Header {
                        version: 1,
                        session: id,
                        sequence: 2,
                    },
                    plan,
                }
                .encode()
                .unwrap(),
                &mut snapshot,
            )
            .unwrap();
        initial.backend.fail_delete = true;
        let mut recovered = Service::recover(
            initial.journal,
            initial.backend,
            501,
            peer().verified_requirement,
        )
        .unwrap();
        let header = Header {
            version: 1,
            session: SessionId::new(),
            sequence: 1,
        };
        assert!(matches!(
            recovered
                .request(
                    &peer(),
                    &Request::Status { header }.encode().unwrap(),
                    &mut snapshot
                )
                .unwrap()
                .result,
            ReplyResult::Status {
                active: false,
                cleanup_required: true
            }
        ));
        assert!(
            recovered
                .request(
                    &peer(),
                    &request(1, SessionId::new()).encode().unwrap(),
                    &mut snapshot
                )
                .is_err()
        );
        recovered.backend.fail_delete = false;
        assert!(matches!(
            recovered
                .request(
                    &peer(),
                    &Request::Clear { header }.encode().unwrap(),
                    &mut snapshot
                )
                .unwrap()
                .result,
            ReplyResult::Cleared
        ));
        assert!(recovered.backend.routes.is_empty());
    }
    #[test]
    fn unauthorized_peer_never_reaches_attestation_or_routes() {
        let (_dir, mut s) = service();
        for wrong in [
            PeerIdentity { uid: 502, ..peer() },
            PeerIdentity {
                verified_requirement: "other code".into(),
                ..peer()
            },
        ] {
            rejected(s.request(
                &wrong,
                &request(1, SessionId::new()).encode().unwrap(),
                &mut |_, _| panic!("unauthorized attestation"),
            ));
        }
        assert_eq!(s.backend.mutations, 0);
    }
    #[test]
    fn session_replay_concurrent_client_and_interface_substitution_are_rejected() {
        let (_dir, mut s) = service();
        let id = SessionId::new();
        let begin = request(1, id);
        assert!(matches!(
            s.request(&peer(), &begin.encode().unwrap(), &mut snapshot)
                .unwrap()
                .result,
            ReplyResult::Begun
        ));
        rejected(s.request(&peer(), &begin.encode().unwrap(), &mut snapshot));
        rejected(s.request(
            &PeerIdentity {
                connection: 2,
                ..peer()
            },
            &request(1, SessionId::new()).encode().unwrap(),
            &mut snapshot,
        ));
        rejected(
            s.request(
                &peer(),
                &Request::Clear {
                    header: Header {
                        version: 1,
                        session: SessionId::new(),
                        sequence: 2,
                    },
                }
                .encode()
                .unwrap(),
                &mut snapshot,
            ),
        );
        let Request::Begin {
            profile_id,
            vpn_interface,
            ..
        } = begin
        else {
            unreachable!()
        };
        let mut plan = RoutePlan {
            profile_id,
            vpn_interface,
            routes: vec![Route {
                destination: "198.51.100.7".parse().unwrap(),
                prefix: 32,
                interface_id: 9,
                gateway: None,
            }],
            warnings: vec![],
        };
        plan.vpn_interface.index = 8;
        plan.routes[0].interface_id = 8;
        rejected(
            s.request(
                &peer(),
                &Request::Apply {
                    header: Header {
                        version: 1,
                        session: id,
                        sequence: 2,
                    },
                    plan,
                }
                .encode()
                .unwrap(),
                &mut snapshot,
            ),
        );
        assert_eq!(s.backend.mutations, 0);
    }
    #[test]
    fn malformed_frames_and_unverified_split_mode_never_mutate() {
        let (_dir, mut s) = service();
        for bytes in [
            b"{".to_vec(),
            vec![b' '; 1_048_577],
            b"{\"operation\":\"execute\"}".to_vec(),
        ] {
            rejected(s.request(&peer(), &bytes, &mut snapshot));
        }
        rejected(s.request(
            &peer(),
            &request(1, SessionId::new()).encode().unwrap(),
            &mut |p, v| {
                let mut s = snapshot(p, v)?;
                s.split_enabled = false;
                Ok(s)
            },
        ));
        assert_eq!(s.backend.mutations, 0);
    }
    #[test]
    fn every_apply_rechecks_native_interface_and_physical_paths() {
        for case in 0..3 {
            let (_dir, mut service) = service();
            let id = SessionId::new();
            let begin = request(1, id);
            service
                .request(&peer(), &begin.encode().unwrap(), &mut snapshot)
                .unwrap();
            let Request::Begin {
                profile_id,
                vpn_interface,
                ..
            } = begin
            else {
                unreachable!()
            };
            let plan = RoutePlan {
                profile_id,
                vpn_interface,
                routes: vec![Route {
                    destination: "198.51.100.7".parse().unwrap(),
                    prefix: 32,
                    interface_id: 9,
                    gateway: None,
                }],
                warnings: vec![],
            };
            let request = Request::Apply {
                header: Header {
                    version: 1,
                    session: id,
                    sequence: 2,
                },
                plan,
            };
            rejected(
                service.request(&peer(), &request.encode().unwrap(), &mut |p, v| {
                    let mut s = snapshot(p, v)?;
                    match case {
                        0 => s.vpn.index = 8,
                        1 => s.default_interface_id = 9,
                        _ => s.endpoint_interface_id = 9,
                    }
                    Ok(s)
                }),
            );
            assert_eq!(service.backend.mutations, 0);
        }
    }

    #[test]
    fn clear_after_connection_loss_is_idempotent_when_recovery_already_finished() {
        let (_dir, mut service) = service();
        let id = SessionId::new();
        service
            .request(&peer(), &request(1, id).encode().unwrap(), &mut snapshot)
            .unwrap();
        service.invalidated(1).unwrap();
        let new_peer = PeerIdentity {
            connection: 2,
            ..peer()
        };
        let clear = Request::Clear {
            header: Header {
                version: 1,
                session: id,
                sequence: 2,
            },
        };
        assert!(matches!(
            service
                .request(&new_peer, &clear.encode().unwrap(), &mut snapshot)
                .unwrap()
                .result,
            ReplyResult::Cleared
        ));
        assert_eq!(service.backend.mutations, 0);
    }

    #[test]
    fn failed_invalidation_allows_new_authenticated_session_to_retry_cleanup() {
        let (_dir, mut s) = service();
        let id = SessionId::new();
        let begin = request(1, id);
        s.request(&peer(), &begin.encode().unwrap(), &mut snapshot)
            .unwrap();
        let Request::Begin {
            profile_id,
            vpn_interface,
            ..
        } = begin
        else {
            unreachable!()
        };
        let plan = RoutePlan {
            profile_id,
            vpn_interface,
            routes: vec![Route {
                destination: "198.51.100.7".parse().unwrap(),
                prefix: 32,
                interface_id: 9,
                gateway: None,
            }],
            warnings: vec![],
        };
        let r = Request::Apply {
            header: Header {
                version: 1,
                session: id,
                sequence: 2,
            },
            plan,
        };
        assert!(matches!(
            s.request(&peer(), &r.encode().unwrap(), &mut snapshot)
                .unwrap()
                .result,
            ReplyResult::Applied
        ));
        s.backend.fail_delete = true;
        assert!(matches!(s.invalidated(1), Err(Error::Cleanup)));
        let new_peer = PeerIdentity {
            connection: 2,
            ..peer()
        };
        let header = Header {
            version: 1,
            session: SessionId::new(),
            sequence: 1,
        };
        assert!(matches!(
            s.request(
                &new_peer,
                &Request::Status { header }.encode().unwrap(),
                &mut snapshot
            )
            .unwrap()
            .result,
            ReplyResult::Status {
                active: false,
                cleanup_required: true
            }
        ));
        rejected(s.request(
            &new_peer,
            &request(1, SessionId::new()).encode().unwrap(),
            &mut snapshot,
        ));
        s.backend.fail_delete = false;
        let wrong_peer = PeerIdentity {
            uid: 502,
            ..new_peer.clone()
        };
        rejected(s.request(
            &wrong_peer,
            &Request::Clear { header }.encode().unwrap(),
            &mut snapshot,
        ));
        assert_eq!(s.backend.routes.len(), 1);
        assert!(matches!(
            s.request(
                &new_peer,
                &Request::Clear { header }.encode().unwrap(),
                &mut snapshot
            )
            .unwrap()
            .result,
            ReplyResult::Cleared
        ));
        assert!(s.backend.routes.is_empty());
        assert!(!s.has_open_session());
    }
    #[test]
    fn invalidation_cleans_only_its_bound_connection() {
        let (_dir, mut s) = service();
        let id = SessionId::new();
        let begin = request(1, id);
        s.request(&peer(), &begin.encode().unwrap(), &mut snapshot)
            .unwrap();
        let Request::Begin {
            profile_id,
            vpn_interface,
            ..
        } = begin
        else {
            unreachable!()
        };
        let plan = RoutePlan {
            profile_id,
            vpn_interface,
            routes: vec![Route {
                destination: "198.51.100.7".parse().unwrap(),
                prefix: 32,
                interface_id: 9,
                gateway: None,
            }],
            warnings: vec![],
        };
        let r = Request::Apply {
            header: Header {
                version: 1,
                session: id,
                sequence: 2,
            },
            plan,
        };
        assert!(matches!(
            s.request(&peer(), &r.encode().unwrap(), &mut snapshot)
                .unwrap()
                .result,
            ReplyResult::Applied
        ));
        s.invalidated(2).unwrap();
        assert_eq!(s.backend.routes.len(), 1);
        s.invalidated(1).unwrap();
        assert!(s.backend.routes.is_empty());
    }
}
