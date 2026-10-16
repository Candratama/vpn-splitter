use crate::{
    Error,
    config::Config,
    protocol::{FaultCode, Header, Reply, Request},
    routing::RoutePlan,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ProfileId(String);
impl ProfileId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for ProfileId {
    type Error = Error;
    fn try_from(value: String) -> Result<Self, Error> {
        Config {
            profile_id: Some(value.clone()),
            ..Config::default()
        }
        .normalized()?;
        Ok(Self(value))
    }
}
impl From<ProfileId> for String {
    fn from(value: ProfileId) -> String {
        value.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SessionId(uuid::Uuid);
impl SessionId {
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4())
    }
    pub fn is_nil(&self) -> bool {
        self.0.is_nil()
    }
}
impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    Disconnected,
    Connecting,
    Applying,
    Active,
    Disconnecting,
    CleanupRequired,
}

#[derive(Debug)]
pub enum Event {
    Connect {
        profile: ProfileId,
        already_connected: bool,
        now_ms: u64,
    },
    Preflight {
        session: SessionId,
        result: Result<(), FaultCode>,
        now_ms: u64,
    },
    Connected {
        session: SessionId,
        result: Result<(), FaultCode>,
        now_ms: u64,
    },
    Planned {
        session: SessionId,
        revision: u64,
        result: Result<RoutePlan, FaultCode>,
        now_ms: u64,
    },
    Helper {
        reply: Reply,
        now_ms: u64,
    },
    Refresh {
        now_ms: u64,
    },
    Cancel {
        session: SessionId,
        now_ms: u64,
    },
    NetworkChanged {
        session: SessionId,
        now_ms: u64,
    },
    VpnLost {
        session: SessionId,
        now_ms: u64,
    },
    Disconnected {
        session: SessionId,
        result: Result<(), FaultCode>,
        now_ms: u64,
    },
    Tick {
        session: SessionId,
        now_ms: u64,
    },
    RetryCleanup {
        now_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Effect {
    Preflight {
        session: SessionId,
        profile: ProfileId,
    },
    ConnectVpn {
        session: SessionId,
        profile: ProfileId,
    },
    ResolvePlan {
        session: SessionId,
        revision: u64,
        profile: ProfileId,
    },
    Helper(Request),
    DisconnectVpn {
        session: SessionId,
        profile: ProfileId,
    },
    Notify {
        session: SessionId,
        code: FaultCode,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Idle,
    Preflight,
    Connecting,
    Planning,
    Beginning(u64),
    Applying(u64),
    Clearing(u64),
    Disconnecting,
}
struct Context {
    id: SessionId,
    profile: ProfileId,
    owns_vpn: bool,
    connected: bool,
    helper_started: bool,
    current: Option<RoutePlan>,
    pending: Option<RoutePlan>,
    revision: u64,
    sequence: u64,
    refresh_pending: bool,
}
pub struct Session {
    state: SessionState,
    phase: Phase,
    deadline: Option<u64>,
    context: Option<Context>,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            state: SessionState::Disconnected,
            phase: Phase::Idle,
            deadline: None,
            context: None,
        }
    }
}
impl Session {
    pub fn state(&self) -> SessionState {
        self.state
    }
    pub fn id(&self) -> Option<SessionId> {
        self.context.as_ref().map(|c| c.id)
    }
    pub fn active_plan(&self) -> Option<&RoutePlan> {
        self.context.as_ref().and_then(|c| c.current.as_ref())
    }
    fn finish(&mut self) {
        *self = Self::default();
    }
    fn resolve(&mut self, now: u64) -> Vec<Effect> {
        let c = self.context.as_mut().unwrap();
        c.revision += 1;
        c.refresh_pending = false;
        self.phase = Phase::Planning;
        self.deadline = Some(now.saturating_add(30_000));
        vec![Effect::ResolvePlan {
            session: c.id,
            revision: c.revision,
            profile: c.profile.clone(),
        }]
    }
    fn header(&mut self) -> Header {
        let c = self.context.as_mut().unwrap();
        c.sequence += 1;
        Header {
            version: 1,
            session: c.id,
            sequence: c.sequence,
        }
    }
    fn apply(&mut self, now: u64) -> Vec<Effect> {
        let header = self.header();
        self.phase = Phase::Applying(header.sequence);
        self.state = SessionState::Applying;
        self.deadline = Some(now.saturating_add(10_000));
        vec![Effect::Helper(Request::Apply {
            header,
            plan: self.context.as_ref().unwrap().pending.clone().unwrap(),
        })]
    }
    fn cleanup(&mut self, now: u64) -> Vec<Effect> {
        let c = self.context.as_mut().unwrap();
        c.refresh_pending = false;
        c.pending = None;
        self.state = SessionState::Disconnecting;
        if c.helper_started {
            let header = self.header();
            self.phase = Phase::Clearing(header.sequence);
            self.deadline = Some(now.saturating_add(10_000));
            vec![Effect::Helper(Request::Clear { header })]
        } else if c.owns_vpn && c.connected {
            self.phase = Phase::Disconnecting;
            self.deadline = Some(now.saturating_add(30_000));
            vec![Effect::DisconnectVpn {
                session: c.id,
                profile: c.profile.clone(),
            }]
        } else {
            self.finish();
            vec![]
        }
    }
    fn fail(&mut self, code: FaultCode, now: u64) -> Vec<Effect> {
        let mut effects = vec![Effect::Notify {
            session: self.id().unwrap(),
            code,
        }];
        effects.extend(self.cleanup(now));
        effects
    }
    fn active(&mut self, now: u64) -> Vec<Effect> {
        self.state = SessionState::Active;
        self.phase = Phase::Idle;
        self.deadline = None;
        if self.context.as_ref().unwrap().refresh_pending {
            self.resolve(now)
        } else {
            vec![]
        }
    }
    pub fn handle(&mut self, event: Event) -> Vec<Effect> {
        use crate::protocol::ReplyResult;
        if let Event::Connect {
            profile,
            already_connected,
            now_ms,
        } = event
        {
            if self.state != SessionState::Disconnected {
                return vec![];
            }
            let id = SessionId::new();
            self.context = Some(Context {
                id,
                profile: profile.clone(),
                owns_vpn: !already_connected,
                connected: already_connected,
                helper_started: false,
                current: None,
                pending: None,
                revision: 0,
                sequence: 0,
                refresh_pending: false,
            });
            self.state = SessionState::Connecting;
            self.phase = Phase::Preflight;
            self.deadline = Some(now_ms.saturating_add(30_000));
            return vec![Effect::Preflight {
                session: id,
                profile,
            }];
        }
        let Some(id) = self.id() else {
            return vec![];
        };
        let event_id = match &event {
            Event::Preflight { session, .. }
            | Event::Connected { session, .. }
            | Event::Planned { session, .. }
            | Event::Cancel { session, .. }
            | Event::NetworkChanged { session, .. }
            | Event::VpnLost { session, .. }
            | Event::Disconnected { session, .. }
            | Event::Tick { session, .. } => Some(*session),
            Event::Helper { reply, .. } => Some(reply.header.session),
            _ => None,
        };
        if event_id.is_some_and(|other| other != id) {
            return vec![];
        }
        match event {
            Event::Preflight { result, now_ms, .. } if self.phase == Phase::Preflight => {
                if let Err(code) = result {
                    return self.fail(code, now_ms);
                }
                let c = self.context.as_mut().unwrap();
                if !c.owns_vpn {
                    return self.resolve(now_ms);
                }
                c.connected = true;
                self.phase = Phase::Connecting;
                self.deadline = Some(now_ms.saturating_add(30_000));
                vec![Effect::ConnectVpn {
                    session: id,
                    profile: c.profile.clone(),
                }]
            }
            Event::Connected { result, now_ms, .. } if self.phase == Phase::Connecting => {
                match result {
                    Ok(()) => self.resolve(now_ms),
                    Err(code) => self.fail(code, now_ms),
                }
            }
            Event::Planned {
                revision,
                result,
                now_ms,
                ..
            } if self.phase == Phase::Planning
                && revision == self.context.as_ref().unwrap().revision =>
            {
                let c = self.context.as_ref().unwrap();
                let result = result.and_then(|plan| {
                    if plan.profile_id != c.profile
                        || plan.validate().is_err()
                        || c.current
                            .as_ref()
                            .is_some_and(|old| old.vpn_interface != plan.vpn_interface)
                    {
                        Err(FaultCode::InvalidInput)
                    } else {
                        Ok(plan)
                    }
                });
                match result {
                    Err(code) if c.current.is_some() => {
                        let mut effects = self.active(now_ms);
                        effects.push(Effect::Notify { session: id, code });
                        effects
                    }
                    Err(code) => self.fail(code, now_ms),
                    Ok(plan) => {
                        let c = self.context.as_mut().unwrap();
                        c.pending = Some(plan.clone());
                        if c.helper_started {
                            return self.apply(now_ms);
                        }
                        c.helper_started = true;
                        let header = self.header();
                        self.phase = Phase::Beginning(header.sequence);
                        self.state = SessionState::Applying;
                        self.deadline = Some(now_ms.saturating_add(10_000));
                        vec![Effect::Helper(Request::Begin {
                            header,
                            profile_id: plan.profile_id,
                            vpn_interface: plan.vpn_interface,
                        })]
                    }
                }
            }
            Event::Helper { reply, now_ms } => {
                if reply.header.validate().is_err() {
                    return vec![];
                }
                let sequence = match self.phase {
                    Phase::Beginning(n) | Phase::Applying(n) | Phase::Clearing(n) => n,
                    _ => return vec![],
                };
                if reply.header.sequence != sequence {
                    return vec![];
                }
                match (self.phase, reply.result) {
                    (Phase::Beginning(_), ReplyResult::Begun) => self.apply(now_ms),
                    (Phase::Applying(_), ReplyResult::Applied) => {
                        let c = self.context.as_mut().unwrap();
                        c.current = c.pending.take();
                        self.active(now_ms)
                    }
                    (Phase::Clearing(_), ReplyResult::Cleared) => {
                        let c = self.context.as_mut().unwrap();
                        c.helper_started = false;
                        c.current = None;
                        self.cleanup(now_ms)
                    }
                    (Phase::Clearing(_), ReplyResult::Rejected { code, .. }) => {
                        self.state = SessionState::CleanupRequired;
                        self.phase = Phase::Idle;
                        self.deadline = None;
                        vec![Effect::Notify { session: id, code }]
                    }
                    (
                        Phase::Applying(_),
                        ReplyResult::Rejected {
                            code,
                            previous_plan_restored: true,
                        },
                    ) if self.context.as_ref().unwrap().current.is_some() => {
                        self.context.as_mut().unwrap().pending = None;
                        let mut effects = self.active(now_ms);
                        effects.push(Effect::Notify { session: id, code });
                        effects
                    }
                    (_, ReplyResult::Rejected { code, .. }) => {
                        let effects = self.fail(code, now_ms);
                        self.state = SessionState::CleanupRequired;
                        effects
                    }
                    _ => vec![],
                }
            }
            Event::Refresh { now_ms }
                if self.context.as_ref().unwrap().current.is_some()
                    && matches!(
                        self.phase,
                        Phase::Idle | Phase::Planning | Phase::Applying(_)
                    )
                    && self.state != SessionState::CleanupRequired =>
            {
                if matches!(self.phase, Phase::Applying(_)) {
                    self.context.as_mut().unwrap().refresh_pending = true;
                    vec![]
                } else {
                    self.resolve(now_ms)
                }
            }
            Event::Cancel { now_ms, .. } | Event::NetworkChanged { now_ms, .. } => {
                if matches!(self.phase, Phase::Clearing(_) | Phase::Disconnecting) {
                    vec![]
                } else {
                    self.cleanup(now_ms)
                }
            }
            Event::VpnLost { now_ms, .. } => {
                self.context.as_mut().unwrap().connected = false;
                if matches!(self.phase, Phase::Clearing(_)) {
                    vec![]
                } else {
                    self.cleanup(now_ms)
                }
            }
            Event::Disconnected { result, .. } if self.phase == Phase::Disconnecting => {
                match result {
                    Ok(()) => {
                        self.finish();
                        vec![]
                    }
                    Err(code) => {
                        self.state = SessionState::CleanupRequired;
                        self.phase = Phase::Idle;
                        self.deadline = None;
                        vec![Effect::Notify { session: id, code }]
                    }
                }
            }
            Event::Tick { now_ms, .. }
                if self.deadline.is_some_and(|deadline| now_ms >= deadline) =>
            {
                if matches!(self.phase, Phase::Clearing(_) | Phase::Disconnecting) {
                    self.state = SessionState::CleanupRequired;
                    self.phase = Phase::Idle;
                    self.deadline = None;
                    vec![Effect::Notify {
                        session: id,
                        code: FaultCode::Timeout,
                    }]
                } else if self.phase == Phase::Planning
                    && self.context.as_ref().unwrap().current.is_some()
                {
                    let mut effects = self.active(now_ms);
                    effects.push(Effect::Notify {
                        session: id,
                        code: FaultCode::Timeout,
                    });
                    effects
                } else {
                    self.fail(FaultCode::Timeout, now_ms)
                }
            }
            Event::RetryCleanup { now_ms }
                if self.state == SessionState::CleanupRequired && self.phase == Phase::Idle =>
            {
                self.cleanup(now_ms)
            }
            _ => vec![],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{protocol::ReplyResult, routing::tests::plan};
    fn profile() -> ProfileId {
        ProfileId::try_from("Example VPN".to_string()).unwrap()
    }
    fn start(already_connected: bool) -> (Session, SessionId) {
        let mut s = Session::default();
        let effects = s.handle(Event::Connect {
            profile: profile(),
            already_connected,
            now_ms: 0,
        });
        assert!(matches!(effects.as_slice(), [Effect::Preflight { .. }]));
        let id = s.id().unwrap();
        (s, id)
    }
    fn header(effects: &[Effect]) -> Header {
        effects
            .iter()
            .find_map(|e| match e {
                Effect::Helper(r) => Some(r.header()),
                _ => None,
            })
            .expect("helper operation")
    }
    fn preparing(already_connected: bool) -> (Session, SessionId, Header) {
        let (mut s, id) = start(already_connected);
        let effects = s.handle(Event::Preflight {
            session: id,
            result: Ok(()),
            now_ms: 1,
        });
        if !already_connected {
            assert!(matches!(effects.as_slice(), [Effect::ConnectVpn { .. }]));
            s.handle(Event::Connected {
                session: id,
                result: Ok(()),
                now_ms: 2,
            });
        }
        let effects = s.handle(Event::Planned {
            session: id,
            revision: 1,
            result: Ok(plan()),
            now_ms: 3,
        });
        let h = header(&effects);
        assert_eq!(h.sequence, 1);
        (s, id, h)
    }
    fn applying(already_connected: bool) -> (Session, SessionId, Header) {
        let (mut s, id, h) = preparing(already_connected);
        let effects = s.handle(Event::Helper {
            reply: Reply {
                header: h,
                result: ReplyResult::Begun,
            },
            now_ms: 4,
        });
        let h = header(&effects);
        assert_eq!(h.sequence, 2);
        (s, id, h)
    }
    fn active(already_connected: bool) -> (Session, SessionId) {
        let (mut s, id, h) = applying(already_connected);
        s.handle(Event::Helper {
            reply: Reply {
                header: h,
                result: ReplyResult::Applied,
            },
            now_ms: 5,
        });
        assert_eq!(s.state(), SessionState::Active);
        (s, id)
    }

    #[test]
    fn refresh_during_apply_is_coalesced_and_wrong_reply_kind_cannot_activate() {
        let (mut s, id) = active(false);
        s.handle(Event::Refresh { now_ms: 10 });
        let h = header(&s.handle(Event::Planned {
            session: id,
            revision: 2,
            result: Ok(plan()),
            now_ms: 11,
        }));
        assert!(
            s.handle(Event::Helper {
                reply: Reply {
                    header: h,
                    result: ReplyResult::Begun
                },
                now_ms: 12
            })
            .is_empty()
        );
        assert_eq!(s.state(), SessionState::Applying);
        s.handle(Event::Refresh { now_ms: 13 });
        s.handle(Event::Refresh { now_ms: 14 });
        let effects = s.handle(Event::Helper {
            reply: Reply {
                header: h,
                result: ReplyResult::Applied,
            },
            now_ms: 15,
        });
        assert!(matches!(
            effects.as_slice(),
            [Effect::ResolvePlan { revision: 3, .. }]
        ));
    }

    #[test]
    fn duplicate_connect_and_stale_native_callbacks_cannot_replace_a_session() {
        let (mut s, id) = start(false);
        assert!(
            s.handle(Event::Connect {
                profile: profile(),
                already_connected: false,
                now_ms: 1
            })
            .is_empty()
        );
        assert_eq!(s.id(), Some(id));
        assert!(
            s.handle(Event::Connected {
                session: SessionId::new(),
                result: Ok(()),
                now_ms: 2
            })
            .is_empty()
        );
        let effects = s.handle(Event::Preflight {
            session: id,
            result: Err(FaultCode::UnsupportedProfile),
            now_ms: 3,
        });
        assert_eq!(s.state(), SessionState::Disconnected);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::ConnectVpn { .. } | Effect::Helper(_)))
        );
    }

    #[test]
    fn connection_timeout_uses_30_seconds_without_polling_side_effects() {
        let (mut s, id) = start(false);
        assert!(
            s.handle(Event::Tick {
                session: id,
                now_ms: 29_999
            })
            .is_empty()
        );
        let effects = s.handle(Event::Tick {
            session: id,
            now_ms: 30_000,
        });
        assert!(matches!(
            effects.as_slice(),
            [Effect::Notify {
                code: FaultCode::Timeout,
                ..
            }]
        ));
        assert_eq!(s.state(), SessionState::Disconnected);
    }

    #[test]
    fn cancel_during_apply_ignores_late_success_and_clears_before_disconnect() {
        let (mut s, id, h) = applying(false);
        let effects = s.handle(Event::Cancel {
            session: id,
            now_ms: 5,
        });
        let clear = header(&effects);
        assert_eq!(clear.sequence, 3);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::DisconnectVpn { .. }))
        );
        assert!(
            s.handle(Event::Helper {
                reply: Reply {
                    header: h,
                    result: ReplyResult::Applied
                },
                now_ms: 6
            })
            .is_empty()
        );
        let effects = s.handle(Event::Helper {
            reply: Reply {
                header: clear,
                result: ReplyResult::Cleared,
            },
            now_ms: 7,
        });
        assert!(matches!(effects.as_slice(), [Effect::DisconnectVpn { .. }]));
        s.handle(Event::Disconnected {
            session: id,
            result: Ok(()),
            now_ms: 8,
        });
        assert_eq!(s.state(), SessionState::Disconnected);
        s.handle(Event::Connect {
            profile: profile(),
            already_connected: false,
            now_ms: 9,
        });
        assert_ne!(s.id(), Some(id));
        assert!(
            s.handle(Event::Connected {
                session: id,
                result: Ok(()),
                now_ms: 10
            })
            .is_empty()
        );
    }

    #[test]
    fn helper_timeout_attempts_cleanup_once_then_requires_explicit_retry() {
        let (mut s, id, _) = preparing(false);
        assert!(
            s.handle(Event::Tick {
                session: id,
                now_ms: 10_002
            })
            .is_empty()
        );
        let effects = s.handle(Event::Tick {
            session: id,
            now_ms: 10_003,
        });
        let clear = header(&effects);
        let effects = s.handle(Event::Tick {
            session: id,
            now_ms: 20_003,
        });
        assert_eq!(s.state(), SessionState::CleanupRequired);
        assert!(!effects.iter().any(|e| matches!(e, Effect::Helper(_))));
        let next = header(&s.handle(Event::RetryCleanup { now_ms: 20_004 }));
        assert!(next.sequence > clear.sequence);
    }

    #[test]
    fn adopted_vpn_is_preserved_after_owned_routes_are_cleared() {
        let (mut s, id) = active(true);
        let h = header(&s.handle(Event::Cancel {
            session: id,
            now_ms: 10,
        }));
        let effects = s.handle(Event::Helper {
            reply: Reply {
                header: h,
                result: ReplyResult::Cleared,
            },
            now_ms: 11,
        });
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::DisconnectVpn { .. }))
        );
        assert_eq!(s.state(), SessionState::Disconnected);
    }

    #[test]
    fn failed_refresh_resolution_preserves_the_active_plan_without_mutations() {
        let (mut s, id) = active(false);
        s.handle(Event::Refresh { now_ms: 10 });
        let effects = s.handle(Event::Planned {
            session: id,
            revision: 2,
            result: Err(FaultCode::Dns),
            now_ms: 11,
        });
        assert_eq!(s.state(), SessionState::Active);
        assert_eq!(s.active_plan(), Some(&plan()));
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::Helper(_) | Effect::DisconnectVpn { .. }))
        );
    }

    #[test]
    fn failed_replacement_requires_confirmed_restoration_or_cleanup() {
        for restored in [true, false] {
            let (mut s, id) = active(false);
            s.handle(Event::Refresh { now_ms: 10 });
            let mut next = plan();
            next.routes[0].destination = std::net::Ipv4Addr::new(198, 51, 100, 8);
            let h = header(&s.handle(Event::Planned {
                session: id,
                revision: 2,
                result: Ok(next),
                now_ms: 11,
            }));
            let effects = s.handle(Event::Helper {
                reply: Reply {
                    header: h,
                    result: ReplyResult::Rejected {
                        code: FaultCode::Conflict,
                        previous_plan_restored: restored,
                    },
                },
                now_ms: 12,
            });
            if restored {
                assert_eq!(s.state(), SessionState::Active);
                assert_eq!(s.active_plan(), Some(&plan()));
                assert!(!effects.iter().any(|e| matches!(e, Effect::Helper(_))));
            } else {
                assert_eq!(s.state(), SessionState::CleanupRequired);
                assert!(
                    effects
                        .iter()
                        .any(|e| matches!(e, Effect::Helper(Request::Clear { .. })))
                );
            }
        }
    }

    #[test]
    fn new_refresh_revision_discards_old_dns_results() {
        let (mut s, id) = active(false);
        s.handle(Event::Refresh { now_ms: 10 });
        s.handle(Event::Refresh { now_ms: 11 });
        assert!(
            s.handle(Event::Planned {
                session: id,
                revision: 2,
                result: Ok(plan()),
                now_ms: 12
            })
            .is_empty()
        );
        assert!(matches!(
            s.handle(Event::Planned {
                session: id,
                revision: 3,
                result: Ok(plan()),
                now_ms: 13
            })
            .as_slice(),
            [Effect::Helper(Request::Apply { .. })]
        ));
    }

    #[test]
    fn network_loss_cannot_be_reversed_by_an_old_apply_reply() {
        let (mut s, id, h) = applying(false);
        let effects = s.handle(Event::VpnLost {
            session: id,
            now_ms: 5,
        });
        let clear = header(&effects);
        assert!(
            s.handle(Event::Helper {
                reply: Reply {
                    header: h,
                    result: ReplyResult::Applied
                },
                now_ms: 6
            })
            .is_empty()
        );
        let effects = s.handle(Event::Helper {
            reply: Reply {
                header: clear,
                result: ReplyResult::Cleared,
            },
            now_ms: 7,
        });
        assert_eq!(s.state(), SessionState::Disconnected);
        assert!(
            !effects
                .iter()
                .any(|e| matches!(e, Effect::DisconnectVpn { .. }))
        );
    }
}
