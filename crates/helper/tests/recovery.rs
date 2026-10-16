use std::{net::Ipv4Addr, path::PathBuf};
use vpn_splitter_core::{
    Error,
    routing::{NetworkInterface, Route, RoutePlan},
    session::{ProfileId, SessionId},
};
use vpn_splitter_helper::journal::{Journal, RouteBackend, apply, clear, reconcile};

#[derive(Default)]
struct Backend {
    routes: Vec<Route>,
    denied: bool,
    deny_route: Option<Ipv4Addr>,
    crash_add: bool,
    crash_delete: bool,
}
impl RouteBackend for Backend {
    fn read(&mut self, r: &Route) -> Result<Option<Route>, Error> {
        Ok(self
            .routes
            .iter()
            .find(|existing| existing.destination == r.destination && existing.prefix == r.prefix)
            .cloned())
    }
    fn add(&mut self, r: &Route) -> Result<(), Error> {
        if self.read(r)?.is_some() {
            return Err(Error::Conflict);
        }
        self.routes.push(r.clone());
        assert!(!self.crash_add, "simulated crash after add");
        Ok(())
    }
    fn delete(&mut self, r: &Route) -> Result<(), Error> {
        if self.denied || self.deny_route == Some(r.destination) {
            return Err(Error::Permission);
        }
        let index = self
            .routes
            .iter()
            .position(|existing| existing == r)
            .ok_or(Error::Conflict)?;
        self.routes.remove(index);
        assert!(!self.crash_delete, "simulated crash after delete");
        Ok(())
    }
}
fn plan() -> RoutePlan {
    RoutePlan {
        profile_id: ProfileId::try_from("Example VPN".to_owned()).unwrap(),
        vpn_interface: NetworkInterface {
            index: 9,
            address: Ipv4Addr::new(203, 0, 113, 10),
            gateway: None,
        },
        routes: vec![Route {
            destination: Ipv4Addr::new(198, 51, 100, 7),
            prefix: 32,
            interface_id: 9,
            gateway: None,
        }],
        warnings: vec![],
    }
}
fn private_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    dir
}
fn opened() -> (tempfile::TempDir, PathBuf, Journal) {
    let dir = private_dir();
    let path = dir.path().join("journal.json");
    let mut journal = Journal::open(&path).unwrap();
    journal.begin(SessionId::new()).unwrap();
    (dir, path, journal)
}
#[test]
fn owns_only_new_routes_and_reloads_before_cleanup() {
    for preexisting in [false, true] {
        let (_dir, path, mut j) = opened();
        let mut b = Backend::default();
        if preexisting {
            b.routes = plan().routes;
        }
        apply(&mut j, &plan(), &mut b).unwrap();
        drop(j);
        let mut j = Journal::open(&path).unwrap();
        let report = reconcile(&mut j, &mut b).unwrap();
        assert!(report.complete());
        assert_eq!(report.removed, usize::from(!preexisting));
        assert_eq!(b.routes.len(), usize::from(preexisting));
        assert!(!j.has_pending());
    }
}
#[test]
fn conflicts_never_replace_a_foreign_route() {
    let (_dir, _path, mut j) = opened();
    let mut foreign = plan().routes[0].clone();
    foreign.interface_id = 4;
    let mut b = Backend {
        routes: vec![foreign.clone()],
        ..Default::default()
    };
    assert!(matches!(
        apply(&mut j, &plan(), &mut b),
        Err(Error::Conflict)
    ));
    assert_eq!(b.routes, vec![foreign]);
}
#[test]
fn changed_and_missing_routes_are_preserved_or_forgotten() {
    for changed in [false, true] {
        let (_dir, path, mut j) = opened();
        let mut b = Backend::default();
        apply(&mut j, &plan(), &mut b).unwrap();
        drop(j);
        if changed {
            b.routes[0].interface_id = 4;
        } else {
            b.routes.clear();
        }
        let mut j = Journal::open(&path).unwrap();
        let report = clear(&mut j, &mut b).unwrap();
        assert!(report.complete());
        assert_eq!(report.changed, usize::from(changed));
        assert_eq!(report.missing, usize::from(!changed));
        assert_eq!(b.routes.len(), usize::from(changed));
    }
}
#[test]
fn denied_delete_retains_durable_retry_state() {
    let (_dir, path, mut j) = opened();
    let mut b = Backend::default();
    apply(&mut j, &plan(), &mut b).unwrap();
    b.denied = true;
    assert!(!clear(&mut j, &mut b).unwrap().complete());
    drop(j);
    let mut j = Journal::open(&path).unwrap();
    assert!(j.has_pending());
    b.denied = false;
    assert!(clear(&mut j, &mut b).unwrap().complete());
    assert!(b.routes.is_empty());
}
#[test]
fn crashes_after_mutation_are_reconciled_without_guessing_add_ownership() {
    for after_add in [false, true] {
        let (_dir, path, mut j) = opened();
        let mut b = Backend::default();
        if after_add {
            b.crash_add = true;
        } else {
            apply(&mut j, &plan(), &mut b).unwrap();
            b.crash_delete = true;
        }
        assert!(
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                if after_add {
                    let _ = apply(&mut j, &plan(), &mut b);
                } else {
                    let _ = clear(&mut j, &mut b);
                }
            }))
            .is_err()
        );
        drop(j);
        let mut j = Journal::open(&path).unwrap();
        b.crash_add = false;
        b.crash_delete = false;
        let report = reconcile(&mut j, &mut b).unwrap();
        assert_eq!(report.complete(), !after_add);
        assert_eq!(report.uncertain, usize::from(after_add));
        assert_eq!(b.routes.len(), usize::from(after_add));
        assert_eq!(j.has_pending(), after_add);
        if after_add {
            assert!(apply(&mut j, &plan(), &mut b).is_err());
        }
    }
}
#[test]
fn corruption_and_duplicate_open_block_mutations() {
    let (_dir, path, j) = opened();
    assert!(Journal::open(&path).is_err());
    drop(j);
    std::fs::write(&path, b"{broken").unwrap();
    assert!(Journal::open(&path).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"{broken");
}
#[cfg(unix)]
#[test]
fn refuses_symlinks_and_public_storage() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = private_dir();
    let path = dir.path().join("journal.json");
    let other = dir.path().join("other");
    std::fs::write(&other, b"private").unwrap();
    symlink(&other, &path).unwrap();
    assert!(Journal::open(&path).is_err());
    std::fs::remove_file(&path).unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Journal::open(&path).is_err());
}

#[test]
fn rejects_a_conflicting_replacement_before_mutation_and_keeps_previous_plan() {
    let (_dir, path, mut j) = opened();
    let mut b = Backend::default();
    let previous = plan();
    apply(&mut j, &previous, &mut b).unwrap();
    let mut next = previous.clone();
    next.routes[0].destination = Ipv4Addr::new(198, 51, 100, 8);
    let mut foreign = next.routes[0].clone();
    foreign.interface_id = 4;
    b.routes.push(foreign.clone());
    assert!(matches!(apply(&mut j, &next, &mut b), Err(Error::Conflict)));
    assert_eq!(j.committed_plan(), Some(&previous));
    assert_eq!(b.routes, vec![previous.routes[0].clone(), foreign]);
    drop(j);
    let j = Journal::open(&path).unwrap();
    assert_eq!(j.committed_plan(), Some(&previous));
}
#[test]
fn successful_replacement_releases_only_obsolete_owned_routes() {
    let (_dir, path, mut j) = opened();
    let mut b = Backend::default();
    let previous = plan();
    apply(&mut j, &previous, &mut b).unwrap();
    let mut next = previous.clone();
    next.routes[0].destination = Ipv4Addr::new(198, 51, 100, 8);
    apply(&mut j, &next, &mut b).unwrap();
    assert_eq!(b.routes, next.routes);
    drop(j);
    let mut j = Journal::open(&path).unwrap();
    assert_eq!(j.committed_plan(), Some(&next));
    assert!(clear(&mut j, &mut b).unwrap().complete());
    assert!(b.routes.is_empty());
}

#[test]
fn replacement_failure_restores_the_previous_plan_when_operations_allow_it() {
    let (_dir, path, mut j) = opened();
    let mut b = Backend::default();
    let previous = plan();
    apply(&mut j, &previous, &mut b).unwrap();
    b.deny_route = Some(previous.routes[0].destination);
    let mut next = previous.clone();
    next.routes[0].destination = Ipv4Addr::new(198, 51, 100, 8);
    assert!(matches!(
        apply(&mut j, &next, &mut b),
        Err(Error::Permission)
    ));
    assert_eq!(b.routes, previous.routes);
    assert_eq!(j.committed_plan(), Some(&previous));
    drop(j);
    let j = Journal::open(&path).unwrap();
    assert_eq!(j.committed_plan(), Some(&previous));
}
