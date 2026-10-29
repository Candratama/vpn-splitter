#[cfg(target_os = "macos")]
use crate::platform::macos::{self as platform, Profile};
#[cfg(target_os = "windows")]
use crate::platform::windows::{self as platform, Profile};
use std::{
    path::PathBuf,
    sync::mpsc::{self, Receiver, Sender},
    time::{Duration, Instant},
};
use vpn_splitter_core::{
    Error,
    config::{Config, load_config, save_config},
    protocol::{FaultCode, Header, Reply, ReplyResult, Request},
    routing::{RoutePlan, build_plan},
    session::{Effect, Event, ProfileId, Session, SessionId, SessionState},
};

fn selected_profile<'a>(profiles: &'a [Profile], id: Option<&str>) -> Result<&'a Profile, Error> {
    profiles
        .iter()
        .find(|p| Some(p.id.as_str()) == id && p.supported && p.split_enabled && !p.transitioning)
        .ok_or(Error::UnsupportedProfile)
}
fn native_event(state: SessionState, id: SessionId, kind: u32, now_ms: u64) -> Option<Event> {
    if state == SessionState::Disconnected {
        return None;
    }
    match kind {
        1 => Some(Event::Connected {
            session: id,
            result: Ok(()),
            now_ms,
        }),
        2 if state == SessionState::Connecting => Some(Event::Connected {
            session: id,
            result: Err(FaultCode::Permission),
            now_ms,
        }),
        2 if state == SessionState::Disconnecting => Some(Event::Disconnected {
            session: id,
            result: Ok(()),
            now_ms,
        }),
        2 => Some(Event::VpnLost {
            session: id,
            now_ms,
        }),
        // VPN setup itself publishes network notifications. Fresh snapshot and
        // helper attestation still validate the path before installing any route.
        3 if state == SessionState::Active => Some(Event::Refresh { now_ms }),
        3 => None,
        _ => None,
    }
}
fn fault(error: &Error) -> FaultCode {
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
// A directory fsync can fail after atomic replacement. Read back bytes before
// deciding whether the new config is visible; route planning must use those bytes.
fn visible_save_result(
    path: &std::path::Path,
    config: &Config,
    result: Result<(), Error>,
) -> Result<Option<FaultCode>, Error> {
    match result {
        Ok(()) => Ok(None),
        Err(error) if path.is_file() && load_config(path).is_ok_and(|actual| actual == *config) => {
            Ok(Some(fault(&error)))
        }
        Err(error) => Err(error),
    }
}
fn persist_config(path: &std::path::Path, config: &Config) -> Result<Option<FaultCode>, Error> {
    visible_save_result(path, config, save_config(path, config))
}
pub fn message(code: FaultCode) -> &'static str {
    match code {
        FaultCode::InvalidInput => "Check the profile and destination rules.",
        FaultCode::Permission => {
            "VPN or helper access was denied. Check the saved VPN credentials and helper approval."
        }
        FaultCode::Timeout => "Operation timed out. Check VPN status or retry cleanup.",
        FaultCode::Conflict => {
            "Network or routes changed. Disconnect, verify the profile, then retry."
        }
        FaultCode::UnsupportedProfile => {
            "Select a saved L2TP profile with Send all traffic disabled. The server must currently use a literal IPv4 address."
        }
        FaultCode::Dns => "A hostname has no usable IPv4 answer. Previous routes were preserved.",
        FaultCode::Transport => "Cannot reach the helper. Install / approve it, then retry.",
        FaultCode::Cleanup => "Cleanup is incomplete. Keep the app open and choose Retry cleanup.",
        FaultCode::Io => {
            "Local file operation failed. Reload and check the saved configuration before retrying."
        }
    }
}
#[derive(Clone)]
pub struct View {
    pub config: Config,
    pub profiles: Vec<Profile>,
    pub state: SessionState,
    pub preparing: bool,
    pub vpn_connected: bool,
    pub adopted: bool,
    pub error: Option<FaultCode>,
    pub warnings: Vec<String>,
    pub helper_status: i32,
    pub helper_busy: bool,
    pub quit_ready: bool,
    pub config_revision: u64,
    pub config_ready: bool,
}
impl Default for View {
    fn default() -> Self {
        Self {
            config: Config::default(),
            profiles: vec![],
            state: SessionState::Disconnected,
            preparing: false,
            vpn_connected: false,
            adopted: false,
            error: None,
            warnings: vec![],
            helper_status: -1,
            helper_busy: false,
            quit_ready: false,
            config_revision: 0,
            config_ready: false,
        }
    }
}
pub enum Command {
    Connect,
    Disconnect,
    Refresh,
    RetryCleanup,
    ReloadProfiles,
    Save(Config),
    InstallHelper,
    RemoveHelper,
    LocalInstalled,
    HelperLost(SessionId),
    Recovered(Result<(), FaultCode>),
    Quit,
    Event(Event),
    Native {
        session: SessionId,
        kind: u32,
    },
    Prepared {
        generation: u64,
        result: Result<Vec<Profile>, FaultCode>,
    },
    Profiles(Result<Vec<Profile>, FaultCode>),
    HelperStatus(i32),
    WorkerError(FaultCode),
    DnsIdle,
}
enum OsJob {
    Profiles(Option<u64>),
    Connect(SessionId, ProfileId),
    Disconnect(SessionId, ProfileId),
}
enum HelperJob {
    Preflight(SessionId, ProfileId, bool),
    Request(Request),
    Install,
    Remove,
    Recover,
    Close,
}
struct DnsJob {
    session: SessionId,
    revision: u64,
    config: Config,
    profile: ProfileId,
}
pub fn start(directory: PathBuf) -> (Sender<Command>, async_channel::Receiver<View>) {
    let (tx, rx) = mpsc::channel();
    let (views, updates) = async_channel::bounded(1);
    let worker = tx.clone();
    let stale_updates = updates.clone();
    std::thread::spawn(move || run(directory, worker, rx, views, stale_updates));
    (tx, updates)
}
fn run(
    directory: PathBuf,
    tx: Sender<Command>,
    rx: Receiver<Command>,
    updates: async_channel::Sender<View>,
    stale_updates: async_channel::Receiver<View>,
) {
    let origin = Instant::now();
    let path = directory.join("config.json");
    let mut view = View {
        config_revision: 1,
        ..View::default()
    };
    match load_config(&path) {
        Ok(config) => {
            view.config = config;
            view.config_ready = true;
        }
        Err(error) => view.error = Some(fault(&error)),
    }
    let mut session = Session::default();
    let mut subscription = None;
    let mut generation = 0;
    let mut quitting = false;
    let mut refresh_at = None;
    let mut pending_config: Option<Config> = None;
    let mut previous_config: Option<Config> = None;
    let mut latest_revision = 0;
    let mut expected_helper = None;
    let mut expected_apply = false;
    let mut pending_dns = None;
    let mut last_network_event = None;
    let (os_tx, os_rx) = mpsc::channel();
    let input = tx.clone();
    std::thread::spawn(move || {
        for job in os_rx {
            let command = match job {
                OsJob::Profiles(generation) => {
                    let result = platform::list_profiles().map_err(|e| fault(&e));
                    match generation {
                        Some(generation) => Command::Prepared { generation, result },
                        None => Command::Profiles(result),
                    }
                }
                OsJob::Connect(id, profile) => match platform::connect(&profile) {
                    Ok(()) => continue,
                    Err(error) => {
                        eprintln!("vpn_start_failed code={}", error.code());
                        Command::Event(Event::Connected {
                            session: id,
                            result: Err(fault(&error)),
                            now_ms: origin.elapsed().as_millis() as u64,
                        })
                    }
                },
                OsJob::Disconnect(id, profile) => {
                    let result = platform::disconnect(&profile);
                    let still_running = platform::list_profiles()
                        .map(|profiles| {
                            profiles
                                .iter()
                                .any(|p| p.id == profile && (p.connected || p.transitioning))
                        })
                        .unwrap_or(true);
                    match result {
                        Ok(()) if still_running => continue,
                        result => Command::Event(Event::Disconnected {
                            session: id,
                            result: result.map_err(|e| fault(&e)),
                            now_ms: origin.elapsed().as_millis() as u64,
                        }),
                    }
                }
            };
            if input.send(command).is_err() {
                break;
            }
        }
    });
    let (helper_tx, helper_rx) = mpsc::channel();
    let input = tx.clone();
    std::thread::spawn(move || {
        let mut client: Option<platform::HelperClient> = None;
        for job in helper_rx {
            let command = match job {
                HelperJob::Preflight(id, profile, adopted) => {
                    let result = (|| -> Result<(), FaultCode> {
                        let profiles = platform::list_profiles().map_err(|e| fault(&e))?;
                        let p = selected_profile(&profiles, Some(profile.as_str()))
                            .map_err(|e| fault(&e))?;
                        if p.connected != adopted {
                            return Err(FaultCode::Conflict);
                        }
                        if platform::helper_status() != 1 {
                            eprintln!("connect_preflight_failed stage=helper_approval");
                            return Err(FaultCode::Permission);
                        }
                        let mut connection =
                            platform::HelperClient::connect().map_err(|e| fault(&e))?;
                        let events = input.clone();
                        connection.observe_loss(move || {
                            let _ = events.send(Command::HelperLost(id));
                        });
                        client = Some(connection);
                        let reply = client
                            .as_mut()
                            .unwrap()
                            .request(Request::Status {
                                header: Header {
                                    version: 1,
                                    session: id,
                                    sequence: 1,
                                },
                            })
                            .map_err(|e| fault(&e))?;
                        match reply.result {
                            ReplyResult::Status {
                                active: false,
                                cleanup_required: false,
                            } => Ok(()),
                            ReplyResult::Rejected { code, .. } => Err(code),
                            _ => Err(FaultCode::Cleanup),
                        }
                    })();
                    if let Err(code) = &result {
                        eprintln!("connect_preflight_failed code={code:?}");
                        client = None;
                    }
                    Command::Event(Event::Preflight {
                        session: id,
                        result,
                        now_ms: origin.elapsed().as_millis() as u64,
                    })
                }
                HelperJob::Request(request) => {
                    let operation = match &request {
                        Request::Begin { .. } => "begin",
                        Request::Apply { .. } => "apply",
                        Request::Clear { .. } => "clear",
                        Request::Status { .. } => "status",
                    };
                    let header = request.header();
                    let reply = (|| -> Result<Reply, Error> {
                        if client.is_none() {
                            let mut connection = platform::HelperClient::connect()?;
                            let events = input.clone();
                            let id = header.session;
                            connection.observe_loss(move || {
                                let _ = events.send(Command::HelperLost(id));
                            });
                            client = Some(connection);
                        }
                        client.as_mut().unwrap().request(request)
                    })();
                    let reply = match reply {
                        Ok(reply) => reply,
                        Err(error) => {
                            client = None;
                            Reply {
                                header,
                                result: ReplyResult::Rejected {
                                    code: fault(&error),
                                    previous_plan_restored: false,
                                },
                            }
                        }
                    };
                    if let ReplyResult::Rejected { code, .. } = &reply.result {
                        eprintln!("helper_request_failed operation={operation} code={code:?}");
                    }
                    Command::Event(Event::Helper {
                        reply,
                        now_ms: origin.elapsed().as_millis() as u64,
                    })
                }
                HelperJob::Install => {
                    let result = platform::register_helper();
                    if let Err(error) = &result {
                        let _ = input.send(Command::WorkerError(fault(error)));
                    }
                    if result.is_ok() && platform::helper_status() == -1 {
                        Command::LocalInstalled
                    } else {
                        Command::HelperStatus(platform::helper_status())
                    }
                }
                HelperJob::Remove => {
                    client = None;
                    let result = (|| -> Result<(), Error> {
                        let mut connection = platform::HelperClient::connect()?;
                        let reply = connection.request(Request::Status {
                            header: Header {
                                version: 1,
                                session: SessionId::new(),
                                sequence: 1,
                            },
                        })?;
                        if !matches!(
                            reply.result,
                            ReplyResult::Status {
                                active: false,
                                cleanup_required: false
                            }
                        ) {
                            return Err(Error::Cleanup);
                        }
                        drop(connection);
                        platform::unregister_helper()
                    })();
                    if let Err(error) = result {
                        let _ = input.send(Command::WorkerError(fault(&error)));
                    }
                    Command::HelperStatus(platform::helper_status())
                }
                HelperJob::Recover => {
                    let result = (|| -> Result<(), Error> {
                        let mut connection = platform::HelperClient::connect()?;
                        let reply = connection.request(Request::Clear {
                            header: Header {
                                version: 1,
                                session: SessionId::new(),
                                sequence: 1,
                            },
                        })?;
                        if matches!(reply.result, ReplyResult::Cleared) {
                            Ok(())
                        } else {
                            Err(Error::Cleanup)
                        }
                    })();
                    Command::Recovered(result.map_err(|e| fault(&e)))
                }
                HelperJob::Close => {
                    client = None;
                    continue;
                }
            };
            if input.send(command).is_err() {
                break;
            }
        }
    });
    let (dns_tx, dns_rx) = mpsc::sync_channel::<DnsJob>(1);
    let input = tx.clone();
    std::thread::spawn(move || {
        for job in dns_rx {
            let result = (|| -> Result<RoutePlan, Error> {
                let hosts = platform::resolve_hosts(&job.config.destinations)?;
                let snapshot = platform::snapshot(&job.profile)?;
                build_plan(&job.config, &snapshot, &hosts)
            })();
            if input
                .send(Command::Event(Event::Planned {
                    session: job.session,
                    revision: job.revision,
                    result: result.map_err(|e| fault(&e)),
                    now_ms: origin.elapsed().as_millis() as u64,
                }))
                .is_err()
            {
                break;
            }
            if input.send(Command::DnsIdle).is_err() {
                break;
            }
        }
    });
    let _ = os_tx.send(OsJob::Profiles(None));
    view.helper_status = platform::helper_status();
    loop {
        view.state = session.state();
        if session.state() == SessionState::Active && refresh_at.is_none() {
            refresh_at = Some(Instant::now() + Duration::from_secs(300));
        }
        if session.state() != SessionState::Active {
            refresh_at = None;
        }
        if session.state() == SessionState::Disconnected {
            subscription = None;
            let _ = helper_tx.send(HelperJob::Close);
            if quitting {
                view.quit_ready = true;
            }
        }
        // One latest snapshot is enough; never accumulate UI updates while hidden.
        if updates.is_full() {
            let _ = stale_updates.try_recv();
            let _ = updates.try_send(view.clone());
        } else {
            let _ = updates.try_send(view.clone());
        }
        if view.quit_ready {
            break;
        }
        let deadline = session
            .deadline_ms()
            .map(|d| Duration::from_millis(d.saturating_sub(origin.elapsed().as_millis() as u64)));
        let refresh = refresh_at.map(|t| t.saturating_duration_since(Instant::now()));
        let timeout = match (deadline, refresh) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let received = match timeout {
            Some(timeout) => rx.recv_timeout(timeout),
            None => rx.recv().map_err(|_| mpsc::RecvTimeoutError::Disconnected),
        };
        let now = origin.elapsed().as_millis() as u64;
        let command = match received {
            Ok(command) => command,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if session.state() == SessionState::Active
                    && refresh_at.is_some_and(|t| Instant::now() >= t)
                {
                    refresh_at = Some(Instant::now() + Duration::from_secs(300));
                    Command::Refresh
                } else if let Some(id) = session.id() {
                    Command::Event(Event::Tick {
                        session: id,
                        now_ms: now,
                    })
                } else {
                    continue;
                }
            }
        };
        let mut effects = vec![];
        match command {
            Command::Connect
                if session.state() == SessionState::Disconnected
                    && !view.preparing
                    && !view.helper_busy
                    && view.config_ready =>
            {
                generation += 1;
                view.preparing = true;
                view.error = None;
                let _ = os_tx.send(OsJob::Profiles(Some(generation)));
            }
            Command::Prepared {
                generation: prepared,
                result,
            } if prepared == generation && view.preparing => {
                view.preparing = false;
                match result.and_then(|profiles| {
                    let profile = selected_profile(&profiles, view.config.profile_id.as_deref())
                        .cloned()
                        .map_err(|e| fault(&e))?;
                    Ok((profiles, profile))
                }) {
                    Ok((profiles, profile)) => {
                        view.profiles = profiles;
                        view.adopted = profile.connected;
                        view.vpn_connected = profile.connected;
                        effects = session.handle(Event::Connect {
                            profile: profile.id.clone(),
                            already_connected: profile.connected,
                            now_ms: now,
                        });
                        let id = session.id().unwrap();
                        let input = tx.clone();
                        match platform::Subscription::new(&profile.id, move |kind| {
                            let _ = input.send(Command::Native { session: id, kind });
                        }) {
                            Ok(handle) => subscription = Some(handle),
                            Err(error) => {
                                effects = session.handle(Event::Preflight {
                                    session: id,
                                    result: Err(fault(&error)),
                                    now_ms: now,
                                })
                            }
                        }
                    }
                    Err(code) => view.error = Some(code),
                }
            }
            Command::Quit if view.helper_busy => {
                view.error = Some(FaultCode::Conflict);
            }
            Command::Disconnect | Command::Quit => {
                if matches!(command, Command::Quit) {
                    quitting = true;
                }
                generation += 1;
                view.preparing = false;
                if let Some(id) = session.id() {
                    effects = session.handle(Event::Cancel {
                        session: id,
                        now_ms: now,
                    });
                }
            }
            Command::Refresh => {
                effects = session.handle(Event::Refresh { now_ms: now });
            }
            Command::RetryCleanup
                if session.state() == SessionState::Disconnected && !view.helper_busy =>
            {
                view.helper_busy = true;
                let _ = helper_tx.send(HelperJob::Recover);
            }
            Command::RetryCleanup => {
                view.error = None;
                effects = session.handle(Event::RetryCleanup { now_ms: now });
            }
            Command::Recovered(result) => {
                view.helper_busy = false;
                view.error = result.err();
            }
            Command::HelperLost(id)
                if session.id() == Some(id)
                    && matches!(
                        session.state(),
                        SessionState::Connecting | SessionState::Applying | SessionState::Active
                    ) =>
            {
                eprintln!("helper_connection_lost state={:?}", session.state());
                view.error = Some(FaultCode::Transport);
                effects = session.handle(Event::Cancel {
                    session: id,
                    now_ms: now,
                });
            }
            Command::ReloadProfiles => {
                let _ = os_tx.send(OsJob::Profiles(None));
                view.helper_status = platform::helper_status();
            }
            Command::Profiles(result) => match result {
                Ok(profiles) => {
                    view.vpn_connected = profiles.iter().any(|p| {
                        Some(p.id.as_str()) == view.config.profile_id.as_deref() && p.connected
                    });
                    view.profiles = profiles;
                }
                Err(code) => view.error = Some(code),
            },
            Command::Save(config) => match config.normalized() {
                Ok(config)
                    if !view.preparing
                        && pending_config.is_none()
                        && matches!(
                            session.state(),
                            SessionState::Disconnected | SessionState::Active
                        )
                        && (session.state() == SessionState::Disconnected
                            || (config.profile_id == view.config.profile_id
                                && config.launch_at_login == view.config.launch_at_login)) =>
                {
                    view.error = None;
                    if session.state() == SessionState::Active {
                        pending_config = Some(config);
                        effects = session.handle(Event::Refresh { now_ms: now });
                    } else {
                        let login_changed = config.launch_at_login != view.config.launch_at_login;
                        if login_changed
                            && let Err(error) = platform::set_login(config.launch_at_login)
                        {
                            view.error = Some(fault(&error));
                            continue;
                        }
                        match persist_config(&path, &config) {
                            Ok(warning) => {
                                view.error = warning;
                                view.config_ready = true;
                                view.config = config;
                                view.config_revision += 1;
                            }
                            Err(error) => {
                                if login_changed {
                                    let _ = platform::set_login(view.config.launch_at_login);
                                }
                                view.config_ready = false;
                                view.error = Some(fault(&error));
                            }
                        }
                    }
                }
                Ok(_) => view.error = Some(FaultCode::Conflict),
                Err(error) => view.error = Some(fault(&error)),
            },
            Command::InstallHelper | Command::RemoveHelper
                if session.state() == SessionState::Disconnected
                    && !view.preparing
                    && !view.helper_busy =>
            {
                view.helper_busy = true;
                view.error = None;
                let _ = helper_tx.send(if matches!(command, Command::RemoveHelper) {
                    HelperJob::Remove
                } else {
                    HelperJob::Install
                });
            }
            Command::LocalInstalled => {
                platform::request_local_relaunch();
                view.helper_busy = false;
                quitting = true;
            }
            Command::HelperStatus(status) => {
                view.helper_busy = false;
                view.helper_status = status;
            }
            Command::WorkerError(code) => view.error = Some(code),
            Command::Native { session: id, kind } if Some(id) == session.id() => {
                eprintln!("native_event kind={kind} state={:?}", session.state());
                if kind == 1 {
                    view.vpn_connected = true;
                }
                if kind == 2 {
                    view.vpn_connected = false;
                }
                if kind == 3 {
                    if last_network_event.is_some_and(|t: u64| now.saturating_sub(t) < 250) {
                        continue;
                    }
                    last_network_event = Some(now);
                }
                if let Some(event) = native_event(session.state(), id, kind, now) {
                    effects = session.handle(event);
                }
            }
            Command::DnsIdle => {
                if let Some(job) = pending_dns.take()
                    && let Err(mpsc::TrySendError::Full(job)) = dns_tx.try_send(job)
                {
                    pending_dns = Some(job);
                }
            }
            Command::Event(mut event) => {
                let matching = match &event {
                    Event::Planned {
                        session: id,
                        revision,
                        ..
                    } => {
                        Some(*id) == session.id()
                            && *revision == latest_revision
                            && matches!(
                                session.state(),
                                SessionState::Connecting | SessionState::Active
                            )
                            && session.deadline_ms().is_some()
                    }
                    _ => false,
                };
                if matching && let Event::Planned { result, .. } = &mut event {
                    if result.is_ok() {
                        if let Some(candidate) = pending_config.take() {
                            match persist_config(&path, &candidate) {
                                Ok(warning) => {
                                    view.error = warning;
                                    previous_config = Some(view.config.clone());
                                    view.config = candidate;
                                    view.config_revision += 1;
                                }
                                Err(error) => *result = Err(fault(&error)),
                            }
                        }
                    } else {
                        pending_config = None;
                    }
                }
                let mut applied = false;
                if let Event::Helper { reply, .. } = &event {
                    if Some(reply.header) != expected_helper {
                        continue;
                    }
                    applied = expected_apply
                        && matches!(reply.result, ReplyResult::Applied)
                        && session.state() == SessionState::Applying;
                }
                effects = session.handle(event);
                if applied && session.state() == SessionState::Active {
                    previous_config = None;
                }
            }
            _ => {}
        }
        for effect in effects {
            match effect {
                Effect::Preflight { session, profile } => {
                    let _ = helper_tx.send(HelperJob::Preflight(session, profile, view.adopted));
                }
                Effect::ConnectVpn { session, profile } => {
                    let _ = os_tx.send(OsJob::Connect(session, profile));
                }
                Effect::DisconnectVpn { session, profile } => {
                    let _ = os_tx.send(OsJob::Disconnect(session, profile));
                }
                Effect::ResolvePlan {
                    session,
                    revision,
                    profile,
                } => {
                    latest_revision = revision;
                    let job = DnsJob {
                        session,
                        revision,
                        profile,
                        config: pending_config
                            .clone()
                            .unwrap_or_else(|| view.config.clone()),
                    };
                    if let Err(mpsc::TrySendError::Full(job)) = dns_tx.try_send(job) {
                        pending_dns = Some(job);
                    }
                }
                Effect::Helper(request) => {
                    expected_helper = Some(request.header());
                    expected_apply = matches!(request, Request::Apply { .. });
                    let _ = helper_tx.send(HelperJob::Request(request));
                }
                Effect::Notify { code, .. } => view.error = Some(code),
            }
        }
        if session.deadline_ms().is_none()
            || matches!(
                session.state(),
                SessionState::Disconnected
                    | SessionState::Disconnecting
                    | SessionState::CleanupRequired
            )
        {
            pending_config = None;
        }
        if session.state() != SessionState::Applying
            && let Some(previous) = previous_config.take()
        {
            match persist_config(&path, &previous) {
                Ok(warning) => {
                    view.error = warning.or(view.error);
                    view.config = previous;
                    view.config_revision += 1;
                }
                Err(error) => {
                    view.config_ready = false;
                    view.error = Some(fault(&error));
                    let _ = tx.send(Command::Disconnect);
                }
            }
        }
        view.warnings = if session
            .active_plan()
            .is_some_and(|plan| !plan.warnings.is_empty())
        {
            vec!["Dual-stack destinations: IPv6 may bypass these IPv4 routes.".to_owned()]
        } else {
            vec![]
        };
    }
    drop(subscription);
}

#[cfg(test)]
mod tests {
    use super::*;
    use vpn_splitter_core::{protocol::FaultCode, session::ProfileId};
    fn profile() -> Profile {
        Profile {
            id: ProfileId::try_from("stable-test-id".to_owned()).unwrap(),
            name: "Renamed VPN".into(),
            supported: true,
            split_enabled: true,
            connected: true,
            transitioning: false,
        }
    }
    fn wait_for(rx: &async_channel::Receiver<View>, predicate: impl Fn(&View) -> bool) -> View {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Ok(view) = rx.try_recv()
                && predicate(&view)
            {
                return view;
            }
            assert!(
                Instant::now() < deadline,
                "controller did not publish expected state"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    fn post_replacement_failure_uses_visible_config_and_keeps_warning() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let config = Config {
            profile_id: Some("test-profile".into()),
            ..Config::default()
        };
        save_config(&path, &config).unwrap();
        let failure = || Err(Error::Io(std::io::Error::other("directory sync failed")));
        assert_eq!(
            visible_save_result(&path, &config, failure()).unwrap(),
            Some(FaultCode::Io)
        );
        assert!(visible_save_result(&path, &Config::default(), failure()).is_err());
        assert!(
            visible_save_result(
                &directory.path().join("missing.json"),
                &Config::default(),
                failure()
            )
            .is_err()
        );
    }
    #[test]
    fn vpn_setup_network_notifications_do_not_cancel_pending_connect() {
        for state in [
            SessionState::Connecting,
            SessionState::Applying,
            SessionState::Disconnecting,
        ] {
            assert!(native_event(state, SessionId::new(), 3, 1).is_none());
        }
    }
    #[test]
    fn externally_transitioning_vpn_is_not_adopted_or_claimed() {
        let mut p = profile();
        p.connected = false;
        p.transitioning = true;
        assert!(selected_profile(&[p], Some("stable-test-id")).is_err());
    }
    #[test]
    fn controller_invalid_save_preserves_disk_and_quit_finishes_without_a_vpn() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.json");
        let initial = Config::default();
        save_config(&path, &initial).unwrap();
        let before = std::fs::read(&path).unwrap();
        let (tx, rx) = start(directory.path().to_owned());
        wait_for(&rx, |_| true);
        let mut invalid = initial;
        invalid.destinations = vec![vpn_splitter_core::config::Destination::Cidr {
            network: std::net::Ipv4Addr::UNSPECIFIED,
            prefix: 0,
        }];
        tx.send(Command::Save(invalid)).unwrap();
        wait_for(&rx, |view| view.error == Some(FaultCode::InvalidInput));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        tx.send(Command::Quit).unwrap();
        wait_for(&rx, |view| view.quit_ready);
    }
    #[test]
    fn controller_deleted_profile_does_not_start_a_connection_or_retry_loop() {
        let directory = tempfile::tempdir().unwrap();
        let config = Config {
            profile_id: Some("missing-test-profile".into()),
            ..Config::default()
        };
        save_config(&directory.path().join("config.json"), &config).unwrap();
        let (tx, rx) = start(directory.path().to_owned());
        wait_for(&rx, |_| true);
        tx.send(Command::Connect).unwrap();
        let view = wait_for(&rx, |view| {
            view.error == Some(FaultCode::UnsupportedProfile)
        });
        assert_eq!(view.state, SessionState::Disconnected);
        assert!(!view.preparing);
        tx.send(Command::Quit).unwrap();
        wait_for(&rx, |view| view.quit_ready);
    }

    #[test]
    fn stable_profile_id_survives_rename_and_external_connection_is_adopted() {
        let profiles = vec![profile()];
        let selected = selected_profile(&profiles, Some("stable-test-id")).unwrap();
        assert!(selected.connected);
        assert_eq!(selected.name, "Renamed VPN");
        assert!(selected_profile(&profiles, Some("deleted-id")).is_err());
        for case in 0..2 {
            let mut p = profile();
            if case == 0 {
                p.supported = false;
            } else {
                p.split_enabled = false;
            }
            assert!(selected_profile(&[p], Some("stable-test-id")).is_err());
        }
    }
    #[test]
    fn native_vpn_loss_during_startup_dns_wait_cancels_retry() {
        let mut session = Session::default();
        session.handle(Event::Connect {
            profile: profile().id,
            already_connected: false,
            now_ms: 0,
        });
        let id = session.id().unwrap();
        session.handle(Event::Preflight {
            session: id,
            result: Ok(()),
            now_ms: 1,
        });
        session.handle(native_event(session.state(), id, 1, 2).unwrap());
        session.handle(Event::Planned {
            session: id,
            revision: 1,
            result: Err(FaultCode::Dns),
            now_ms: 3,
        });
        assert_eq!(session.deadline_ms(), Some(1003));
        let effects = session.handle(native_event(session.state(), id, 2, 4).unwrap());
        assert_eq!(session.state(), SessionState::Disconnected);
        assert!(effects.is_empty());
        assert_eq!(session.deadline_ms(), None);
        assert!(
            session
                .handle(Event::Tick {
                    session: id,
                    now_ms: 1003
                })
                .is_empty()
        );
        assert!(
            session
                .handle(Event::Planned {
                    session: id,
                    revision: 1,
                    result: Err(FaultCode::Dns),
                    now_ms: 1004
                })
                .is_empty()
        );
    }

    #[test]
    fn native_failure_loss_and_network_switch_map_to_session_events_without_reconnect() {
        let id = SessionId::new();
        assert!(matches!(
            native_event(SessionState::Connecting, id, 2, 1),
            Some(Event::Connected {
                result: Err(FaultCode::Permission),
                ..
            })
        ));
        assert!(matches!(
            native_event(SessionState::Active, id, 2, 2),
            Some(Event::VpnLost { .. })
        ));
        assert!(matches!(
            native_event(SessionState::Disconnecting, id, 2, 3),
            Some(Event::Disconnected { result: Ok(()), .. })
        ));
        assert!(matches!(
            native_event(SessionState::Active, id, 3, 4),
            Some(Event::Refresh { .. })
        ));
        assert!(native_event(SessionState::Disconnected, id, 1, 5).is_none());
    }
}
