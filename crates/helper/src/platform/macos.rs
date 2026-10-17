use crate::journal::RouteBackend;
use serde::{Deserialize, Serialize};
use std::{ffi::CString, net::Ipv4Addr};
use vpn_splitter_core::{
    Error,
    protocol::MAX_MESSAGE_BYTES,
    routing::{NetworkSnapshot, Route},
    session::ProfileId,
};

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct NativeRoute {
    destination: u32,
    interface_id: u32,
    gateway: u32,
    prefix: u8,
    has_gateway: u8,
    padding: [u8; 2],
}
impl From<&Route> for NativeRoute {
    fn from(r: &Route) -> Self {
        Self {
            destination: r.destination.into(),
            interface_id: r.interface_id,
            gateway: r.gateway.map(u32::from).unwrap_or(0),
            prefix: r.prefix,
            has_gateway: u8::from(r.gateway.is_some()),
            padding: [0; 2],
        }
    }
}
impl From<NativeRoute> for Route {
    fn from(r: NativeRoute) -> Self {
        Self {
            destination: Ipv4Addr::from(r.destination),
            interface_id: r.interface_id,
            gateway: (r.has_gateway != 0).then(|| Ipv4Addr::from(r.gateway)),
            prefix: r.prefix,
        }
    }
}
unsafe extern "C" {
    fn vs_routes(out: *mut NativeRoute, capacity: usize, count: *mut usize) -> i32;
    fn vs_route_change(route: *const NativeRoute, adding: i32) -> i32;
    #[cfg(test)]
    fn vs_route_parser_probe() -> i32;
    fn vs_profiles(out: *mut u8, capacity: usize, length: *mut usize) -> i32;
    fn vs_snapshot(
        profile: *const std::ffi::c_char,
        out: *mut u8,
        capacity: usize,
        length: *mut usize,
    ) -> i32;
    fn vs_vpn_control(profile: *const std::ffi::c_char, connect: i32) -> i32;
}
fn check(code: i32) -> Result<(), Error> {
    match code {
        0 => Ok(()),
        libc::EPERM | libc::EACCES => Err(Error::Permission),
        libc::EEXIST => Err(Error::Conflict),
        libc::ETIMEDOUT => Err(Error::Timeout),
        libc::ENOTSUP | libc::ENOENT => Err(Error::UnsupportedProfile),
        _ => Err(Error::Io(std::io::Error::from_raw_os_error(code))),
    }
}
fn json<T: serde::de::DeserializeOwned>(
    call: impl FnOnce(*mut u8, usize, *mut usize) -> i32,
) -> Result<T, Error> {
    let mut bytes = vec![0; MAX_MESSAGE_BYTES];
    let mut length = 0;
    check(call(bytes.as_mut_ptr(), bytes.len(), &mut length))?;
    if length > bytes.len() {
        return Err(Error::Transport);
    }
    serde_json::from_slice(&bytes[..length]).map_err(|_| Error::Transport)
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: ProfileId,
    pub name: String,
    pub supported: bool,
    pub split_enabled: bool,
    pub connected: bool,
}
pub fn list_profiles() -> Result<Vec<Profile>, Error> {
    json(|out, cap, len| unsafe { vs_profiles(out, cap, len) })
}
pub fn snapshot(profile: &ProfileId) -> Result<NetworkSnapshot, Error> {
    let id = CString::new(profile.as_str())
        .map_err(|_| Error::InvalidInput("Invalid profile".into()))?;
    json(|out, cap, len| unsafe { vs_snapshot(id.as_ptr(), out, cap, len) })
}
pub fn control(profile: &ProfileId, connect: bool) -> Result<(), Error> {
    let id = CString::new(profile.as_str())
        .map_err(|_| Error::InvalidInput("Invalid profile".into()))?;
    check(unsafe { vs_vpn_control(id.as_ptr(), i32::from(connect)) })
}
pub struct MacRouteBackend;
impl MacRouteBackend {
    pub fn routes() -> Result<Vec<Route>, Error> {
        let mut routes = vec![NativeRoute::default(); 65_536];
        let mut count = 0;
        check(unsafe { vs_routes(routes.as_mut_ptr(), routes.len(), &mut count) })?;
        if count > routes.len() {
            return Err(Error::Transport);
        }
        Ok(routes[..count].iter().copied().map(Into::into).collect())
    }
}
impl RouteBackend for MacRouteBackend {
    fn read(&mut self, route: &Route) -> Result<Option<Route>, Error> {
        let matches: Vec<_> = Self::routes()?
            .into_iter()
            .filter(|r| r.destination == route.destination && r.prefix == route.prefix)
            .collect();
        if matches.len() > 1 {
            return Err(Error::Conflict);
        }
        Ok(matches.into_iter().next())
    }
    fn add(&mut self, route: &Route) -> Result<(), Error> {
        route.validate()?;
        if self.read(route)?.is_some() {
            return Err(Error::Conflict);
        }
        check(unsafe { vs_route_change(&NativeRoute::from(route), 1) })
    }
    fn delete(&mut self, route: &Route) -> Result<(), Error> {
        route.validate()?;
        if self.read(route)?.as_ref() != Some(route) {
            return Err(Error::Conflict);
        }
        check(unsafe { vs_route_change(&NativeRoute::from(route), 0) })
    }
}
#[cfg(test)]
mod tests {
    #[test]
    fn unprivileged_helper_start_is_denied_before_storage_or_routes() {
        if unsafe { libc::geteuid() } != 0 {
            assert!(matches!(
                super::serve(),
                Err(vpn_splitter_core::Error::Permission)
            ));
        }
    }
    #[test]
    fn native_xpc_accepts_matching_code_and_rejects_wrong_code() {
        unsafe extern "C" {
            fn vs_xpc_probe() -> i32;
        }
        assert_eq!(unsafe { vs_xpc_probe() }, 0);
    }
    #[test]
    fn native_route_parser_handles_truncation_masks_and_gateway_identity() {
        assert_eq!(unsafe { super::vs_route_parser_probe() }, 0);
    }
    #[test]
    fn native_routes_and_profiles_can_be_read_without_mutation() {
        assert!(!super::MacRouteBackend::routes().unwrap().is_empty());
        super::list_profiles().unwrap();
    }
}

unsafe extern "C" {
    fn vs_console_uid() -> u32;
    fn vs_code_requirement(out: *mut u8, capacity: usize, length: *mut usize) -> i32;
    fn vs_serve(
        context: *mut std::ffi::c_void,
        request: unsafe extern "C" fn(
            *mut std::ffi::c_void,
            u32,
            u64,
            *const std::ffi::c_char,
            *const u8,
            usize,
            *mut u8,
            usize,
            *mut usize,
        ) -> i32,
        invalidated: unsafe extern "C" fn(*mut std::ffi::c_void, u64),
    ) -> i32;
    fn vs_monitor(active: i32) -> i32;
    fn vs_client_create() -> *mut std::ffi::c_void;
    fn vs_client_request(
        client: *mut std::ffi::c_void,
        bytes: *const u8,
        length: usize,
        out: *mut u8,
        capacity: usize,
        out_length: *mut usize,
    ) -> i32;
    fn vs_client_close(client: *mut std::ffi::c_void);
    fn vs_register_helper(unregistering: i32) -> i32;
}
pub struct HelperClient {
    connection: std::ptr::NonNull<std::ffi::c_void>,
}
// NSXPCConnection is thread-safe; this wrapper requires exclusive requests.
unsafe impl Send for HelperClient {}
impl HelperClient {
    pub fn connect() -> Result<Self, Error> {
        let connection =
            std::ptr::NonNull::new(unsafe { vs_client_create() }).ok_or(Error::Permission)?;
        Ok(Self { connection })
    }
    pub fn request(
        &mut self,
        request: vpn_splitter_core::protocol::Request,
    ) -> Result<vpn_splitter_core::protocol::Reply, Error> {
        let bytes = request.encode()?;
        let mut out = vec![0; MAX_MESSAGE_BYTES];
        let mut length = 0;
        check(unsafe {
            vs_client_request(
                self.connection.as_ptr(),
                bytes.as_ptr(),
                bytes.len(),
                out.as_mut_ptr(),
                out.len(),
                &mut length,
            )
        })?;
        if length > out.len() {
            return Err(Error::Transport);
        }
        let reply = vpn_splitter_core::protocol::Reply::decode(&out[..length])?;
        if reply.header != request.header() {
            return Err(Error::Transport);
        }
        Ok(reply)
    }
}
impl Drop for HelperClient {
    fn drop(&mut self) {
        unsafe { vs_client_close(self.connection.as_ptr()) }
    }
}
pub fn register_helper() -> Result<(), Error> {
    check(unsafe { vs_register_helper(0) })
}
pub fn unregister_helper() -> Result<(), Error> {
    check(unsafe { vs_register_helper(1) })
}
type NativeService = crate::service::Service<MacRouteBackend>;
unsafe extern "C" fn handle_request(
    context: *mut std::ffi::c_void,
    uid: u32,
    connection: u64,
    requirement: *const std::ffi::c_char,
    bytes: *const u8,
    length: usize,
    out: *mut u8,
    capacity: usize,
    out_length: *mut usize,
) -> i32 {
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), Error> {
        if length > MAX_MESSAGE_BYTES || capacity > MAX_MESSAGE_BYTES {
            return Err(Error::Transport);
        }
        // The native bridge serializes callbacks on one dispatch queue and owns
        // these buffers for the duration of this call.
        let service = unsafe { &mut *context.cast::<NativeService>() };
        let peer = crate::service::PeerIdentity {
            uid,
            connection,
            verified_requirement: unsafe { std::ffi::CStr::from_ptr(requirement) }
                .to_str()
                .map_err(|_| Error::Permission)?
                .to_owned(),
        };
        let reply = service.request(
            &peer,
            unsafe { std::slice::from_raw_parts(bytes, length) },
            &mut |profile, interface| {
                let state = snapshot(profile)?;
                if state.vpn != interface {
                    return Err(Error::Conflict);
                }
                Ok(state)
            },
        )?;
        if unsafe { vs_monitor(i32::from(service.has_open_session())) } != 0 {
            let _ = service.invalidated(connection);
            return Err(Error::Cleanup);
        }
        let reply = reply.encode()?;
        if reply.len() > capacity {
            return Err(Error::Transport);
        }
        unsafe {
            std::ptr::copy_nonoverlapping(reply.as_ptr(), out, reply.len());
            *out_length = reply.len();
        }
        Ok(())
    }));
    if matches!(result, Ok(Ok(()))) {
        0
    } else {
        libc::EACCES
    }
}
unsafe extern "C" fn invalidated(context: *mut std::ffi::c_void, connection: u64) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let service = unsafe { &mut *context.cast::<NativeService>() };
        if service.invalidated(connection).is_err() {
            eprintln!("Owned-route cleanup requires reconciliation");
        }
        unsafe {
            vs_monitor(i32::from(service.has_open_session()));
        }
    }));
}
pub fn serve() -> Result<(), Error> {
    if unsafe { libc::geteuid() } != 0 {
        return Err(Error::Permission);
    }
    let mut requirement = vec![0; 1024];
    let mut length = 0;
    check(unsafe {
        vs_code_requirement(requirement.as_mut_ptr(), requirement.len(), &mut length)
    })?;
    if length > requirement.len() {
        return Err(Error::Permission);
    }
    let requirement =
        String::from_utf8(requirement[..length].to_vec()).map_err(|_| Error::Permission)?;
    let console_uid = unsafe { vs_console_uid() };
    if console_uid == 0 {
        return Err(Error::Permission);
    }
    use std::os::unix::fs::DirBuilderExt;
    let directory = std::path::Path::new("/Library/Application Support/VPN Splitter");
    match std::fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e.into()),
    }
    let journal = crate::journal::Journal::open(&directory.join("owned-routes.json"))?;
    let mut service = Box::new(NativeService::recover(
        journal,
        MacRouteBackend,
        console_uid,
        requirement,
    )?);
    check(unsafe {
        vs_serve(
            (&mut *service as *mut NativeService).cast(),
            handle_request,
            invalidated,
        )
    })
}
