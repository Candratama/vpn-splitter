use serde::{Deserialize, Serialize};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
use vpn_splitter_core::{
    Error,
    config::MAX_CONFIG_BYTES,
    routing::{Route, RoutePlan},
    session::SessionId,
};

pub trait RouteBackend {
    /// Read the exact destination/prefix, including a conflicting interface/gateway.
    fn read(&mut self, route: &Route) -> Result<Option<Route>, Error>;
    /// Exclusive add: never replace an existing route.
    fn add(&mut self, route: &Route) -> Result<(), Error>;
    /// Delete only the exact identity; native adapters must recheck before deletion.
    fn delete(&mut self, route: &Route) -> Result<(), Error>;
}
#[derive(Debug, Default)]
pub struct CleanupReport {
    pub removed: usize,
    pub missing: usize,
    pub changed: usize,
    pub failed: usize,
    pub uncertain: usize,
}
impl CleanupReport {
    pub fn complete(&self) -> bool {
        self.failed == 0 && self.uncertain == 0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum State {
    Adding,
    Owned,
    Deleting,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    route: Route,
    state: State,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Data {
    version: u8,
    session: Option<SessionId>,
    committed: Option<RoutePlan>,
    incomplete: bool,
    entries: Vec<Entry>,
}
pub struct Journal {
    path: PathBuf,
    _lock: File,
    data: Data,
    poisoned: bool,
    #[cfg(test)]
    fail_write: Option<usize>,
    #[cfg(test)]
    crash_after_write: Option<usize>,
}
fn private_file(path: &Path, create: bool) -> Result<File, Error> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(create);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(Error::Permission);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.mode() & 0o077 != 0
            || metadata.nlink() != 1
        {
            return Err(Error::Permission);
        }
    }
    Ok(file)
}
impl Journal {
    /// Caller selects a fixed helper-owned path, never a path from IPC.
    /// The parent must already be private and protected by the platform installer.
    pub fn open(path: &Path) -> Result<Self, Error> {
        let parent = path.parent().ok_or(Error::Permission)?;
        let metadata = std::fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Permission);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                return Err(Error::Permission);
            }
        }
        let path = parent
            .canonicalize()?
            .join(path.file_name().ok_or(Error::Permission)?);
        let lock = private_file(&path.with_extension("lock"), true)?;
        lock.try_lock().map_err(|_| Error::Conflict)?;
        let data = match private_file(&path, false) {
            Ok(file) => {
                let mut bytes = vec![];
                file.take((MAX_CONFIG_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)?;
                if bytes.len() > MAX_CONFIG_BYTES {
                    return Err(Error::Cleanup);
                }
                let data: Data = serde_json::from_slice(&bytes).map_err(|_| Error::Cleanup)?;
                if data.version != 1
                    || data.entries.len() > 8192
                    || data.session.is_some_and(|id| id.is_nil())
                    || (!data.entries.is_empty() && data.session.is_none())
                {
                    return Err(Error::Cleanup);
                }
                if let Some(plan) = &data.committed {
                    plan.validate().map_err(|_| Error::Cleanup)?;
                }
                let mut seen = std::collections::HashSet::new();
                for entry in &data.entries {
                    entry.route.validate().map_err(|_| Error::Cleanup)?;
                    if !seen.insert((entry.route.destination, entry.route.prefix)) {
                        return Err(Error::Cleanup);
                    }
                }
                data
            }
            Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Data {
                version: 1,
                ..Data::default()
            },
            Err(error) => return Err(error),
        };
        Ok(Self {
            path,
            _lock: lock,
            data,
            poisoned: false,
            #[cfg(test)]
            fail_write: None,
            #[cfg(test)]
            crash_after_write: None,
        })
    }
    fn save(&mut self) -> Result<(), Error> {
        self.ready()?;
        let result = (|| {
            #[cfg(test)]
            if let Some(left) = self.fail_write.as_mut() {
                if *left == 0 {
                    return Err(Error::Io(std::io::Error::other(
                        "injected journal write failure",
                    )));
                }
                *left -= 1;
            }
            let bytes = serde_json::to_vec(&self.data).map_err(|_| Error::Cleanup)?;
            if bytes.len() > MAX_CONFIG_BYTES {
                return Err(Error::Cleanup);
            }
            let parent = self.path.parent().unwrap();
            let mut staging = tempfile::NamedTempFile::new_in(parent)?;
            staging.write_all(&bytes)?;
            staging.as_file().sync_all()?;
            staging
                .persist(&self.path)
                .map_err(|error| Error::Io(error.error))?;
            #[cfg(unix)]
            File::open(parent)?.sync_all()?;
            #[cfg(test)]
            if let Some(left) = self.crash_after_write.as_mut() {
                assert!(*left != 0, "simulated crash after durable journal write");
                *left -= 1;
            }
            Ok(())
        })();
        if result.is_err() {
            self.poisoned = true;
        }
        result
    }
    fn ready(&self) -> Result<(), Error> {
        if self.poisoned {
            Err(Error::Cleanup)
        } else {
            Ok(())
        }
    }
    pub fn begin(&mut self, session: SessionId) -> Result<(), Error> {
        self.ready()?;
        if self.data.session.is_some() || !self.data.entries.is_empty() || session.is_nil() {
            return Err(Error::Conflict);
        }
        self.data.session = Some(session);
        self.save()
    }
    pub fn has_pending(&self) -> bool {
        self.poisoned || !self.data.entries.is_empty()
    }
    pub fn requires_reconciliation(&self) -> bool {
        self.poisoned || self.data.incomplete || uncertain(self)
    }
    pub fn committed_plan(&self) -> Option<&RoutePlan> {
        self.data.committed.as_ref()
    }
    fn add(&mut self, route: &Route, backend: &mut impl RouteBackend) -> Result<(), Error> {
        match backend.read(route)? {
            Some(existing) if existing == *route => return Ok(()),
            Some(_) => return Err(Error::Conflict),
            None => {}
        }
        self.data.entries.push(Entry {
            route: route.clone(),
            state: State::Adding,
        });
        self.save()?;
        backend.add(route)?;
        // Readback precedes the durable ownership claim.
        if backend.read(route)?.as_ref() != Some(route) {
            return Err(Error::Conflict);
        }
        self.data.entries.last_mut().unwrap().state = State::Owned;
        self.save()
    }
    fn remove(&mut self, index: usize, backend: &mut impl RouteBackend) -> Result<(), Error> {
        let route = self.data.entries[index].route.clone();
        self.data.entries[index].state = State::Deleting;
        self.save()?;
        backend.delete(&route)?;
        if backend.read(&route)?.as_ref() == Some(&route) {
            return Err(Error::Cleanup);
        }
        self.data.entries.remove(index);
        self.save()
    }
}
fn uncertain(journal: &Journal) -> bool {
    journal
        .data
        .entries
        .iter()
        .any(|entry| entry.state != State::Owned)
}
pub fn apply(
    journal: &mut Journal,
    plan: &RoutePlan,
    backend: &mut impl RouteBackend,
) -> Result<(), Error> {
    journal.ready()?;
    plan.validate()?;
    if journal.data.session.is_none() || journal.requires_reconciliation() {
        return Err(Error::Cleanup);
    }
    if journal.data.committed.as_ref().is_some_and(|old| {
        old.profile_id != plan.profile_id || old.vpn_interface != plan.vpn_interface
    }) {
        return Err(Error::Conflict);
    }
    // Validate the entire replacement before making the first mutation.
    for route in &plan.routes {
        if backend
            .read(route)?
            .is_some_and(|existing| existing != *route)
        {
            return Err(Error::Conflict);
        }
    }
    let old = journal.data.committed.clone();
    journal.data.incomplete = true;
    journal.save()?;
    match replace(journal, plan, backend) {
        Ok(()) => {
            journal.data.committed = Some(plan.clone());
            journal.data.incomplete = false;
            journal.save()
        }
        Err(error) => {
            if journal.poisoned || settle_failed_operation(journal, backend).is_err() {
                return Err(Error::Cleanup);
            }
            let target = old.clone().unwrap_or(RoutePlan {
                routes: vec![],
                ..plan.clone()
            });
            if replace(journal, &target, backend).is_err() {
                return Err(Error::Cleanup);
            }
            journal.data.committed = old;
            journal.data.incomplete = false;
            journal.save()?;
            Err(error)
        }
    }
}
fn settle_failed_operation(
    journal: &mut Journal,
    backend: &mut impl RouteBackend,
) -> Result<(), Error> {
    let mut index = 0;
    while index < journal.data.entries.len() {
        let entry = journal.data.entries[index].clone();
        if entry.state == State::Owned {
            index += 1;
            continue;
        }
        match backend.read(&entry.route)? {
            Some(_) if entry.state == State::Adding => return Err(Error::Cleanup),
            Some(current) if current == entry.route => {
                journal.data.entries[index].state = State::Owned;
                index += 1;
            }
            _ => {
                journal.data.entries.remove(index);
            }
        }
        journal.save()?;
    }
    Ok(())
}
fn replace(
    journal: &mut Journal,
    plan: &RoutePlan,
    backend: &mut impl RouteBackend,
) -> Result<(), Error> {
    for route in &plan.routes {
        if !journal
            .data
            .entries
            .iter()
            .any(|entry| entry.route == *route)
        {
            journal.add(route, backend)?;
        } else if backend.read(route)?.as_ref() != Some(route) {
            return Err(Error::Conflict);
        }
    }
    let mut index = 0;
    while index < journal.data.entries.len() {
        let route = &journal.data.entries[index].route;
        if plan.routes.contains(route) {
            index += 1;
            continue;
        }
        if backend.read(route)?.as_ref() != Some(route) {
            return Err(Error::Conflict);
        }
        journal.remove(index, backend)?;
    }
    Ok(())
}
pub fn clear(
    journal: &mut Journal,
    backend: &mut impl RouteBackend,
) -> Result<CleanupReport, Error> {
    journal.ready()?;
    if !journal.data.entries.is_empty() && !journal.data.incomplete {
        journal.data.incomplete = true;
        journal.save()?;
    }
    let mut report = CleanupReport::default();
    let mut index = 0;
    while index < journal.data.entries.len() {
        let entry = journal.data.entries[index].clone();
        match backend.read(&entry.route) {
            Err(_) => {
                report.failed += 1;
                index += 1;
                continue;
            }
            Ok(None) => report.missing += 1,
            Ok(Some(current)) if current != entry.route => report.changed += 1,
            Ok(Some(_)) if entry.state == State::Adding => {
                report.uncertain += 1;
                index += 1;
                continue;
            }
            Ok(Some(_)) => match journal.remove(index, backend) {
                Ok(()) => {
                    report.removed += 1;
                    continue;
                }
                Err(error) if journal.poisoned => return Err(error),
                Err(_) => {
                    report.failed += 1;
                    index += 1;
                    continue;
                }
            },
        }
        journal.data.entries.remove(index);
        journal.save()?;
    }
    if report.complete() {
        journal.data.session = None;
        journal.data.committed = None;
        journal.data.incomplete = false;
        journal.save()?;
    }
    Ok(report)
}
pub fn reconcile(
    journal: &mut Journal,
    backend: &mut impl RouteBackend,
) -> Result<CleanupReport, Error> {
    clear(journal, backend)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Backend(Option<Route>);
    impl RouteBackend for Backend {
        fn read(&mut self, _: &Route) -> Result<Option<Route>, Error> {
            Ok(self.0.clone())
        }
        fn add(&mut self, r: &Route) -> Result<(), Error> {
            self.0 = Some(r.clone());
            Ok(())
        }
        fn delete(&mut self, _: &Route) -> Result<(), Error> {
            self.0 = None;
            Ok(())
        }
    }
    fn plan() -> RoutePlan {
        use vpn_splitter_core::{routing::NetworkInterface, session::ProfileId};
        RoutePlan {
            profile_id: ProfileId::try_from("Example VPN".to_owned()).unwrap(),
            vpn_interface: NetworkInterface {
                index: 9,
                address: "203.0.113.10".parse().unwrap(),
                gateway: None,
            },
            routes: vec![Route {
                destination: "198.51.100.7".parse().unwrap(),
                prefix: 32,
                interface_id: 9,
                gateway: None,
            }],
            warnings: vec![],
        }
    }
    #[test]
    fn reload_after_every_durable_write_or_write_failure() {
        for clearing in [false, true] {
            for crash in [false, true] {
                for point in 0..4 {
                    let dir = tempfile::tempdir().unwrap();
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::set_permissions(
                            dir.path(),
                            std::fs::Permissions::from_mode(0o700),
                        )
                        .unwrap();
                    }
                    let path = dir.path().join("journal.json");
                    let mut j = Journal::open(&path).unwrap();
                    j.begin(SessionId::new()).unwrap();
                    let mut b = Backend::default();
                    if clearing {
                        apply(&mut j, &plan(), &mut b).unwrap();
                    }
                    if crash {
                        j.crash_after_write = Some(point);
                    } else {
                        j.fail_write = Some(point);
                    }
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        if clearing {
                            clear(&mut j, &mut b).map(|_| ())
                        } else {
                            apply(&mut j, &plan(), &mut b)
                        }
                    }));
                    if crash {
                        assert!(result.is_err());
                    } else {
                        assert!(result.unwrap().is_err());
                        assert!(j.ready().is_err());
                    }
                    drop(j);
                    let mut j = Journal::open(&path).unwrap();
                    if !clearing && crash && point < 3 {
                        assert!(
                            j.requires_reconciliation(),
                            "unfinished replacement must not claim prior plan restored"
                        );
                    }
                    let report = reconcile(&mut j, &mut b).unwrap();
                    if report.complete() {
                        assert!(b.0.is_none());
                        assert!(!j.has_pending());
                    } else {
                        assert_eq!(report.uncertain, 1);
                        assert!(j.has_pending());
                        assert!(b.0.is_some());
                    }
                }
            }
        }
    }
}
