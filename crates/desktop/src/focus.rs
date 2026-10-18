use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::unix::{
        fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt},
        io::AsRawFd,
        net::{UnixListener, UnixStream},
    },
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::JoinHandle,
    time::Duration,
};
#[cfg(unix)]
pub struct FocusServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}
#[cfg(unix)]
fn private_directory(directory: &Path) -> std::io::Result<PathBuf> {
    let path = directory.join("ipc");
    match std::fs::DirBuilder::new().mode(0o700).create(&path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.is_dir()
        || meta.file_type().is_symlink()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    Ok(path)
}
#[cfg(unix)]
fn same_user(stream: &UnixStream) -> bool {
    let (mut uid, mut gid) = (0, 0);
    unsafe {
        libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0 && uid == libc::geteuid()
    }
}
#[cfg(unix)]
pub fn serve(directory: &Path, tx: async_channel::Sender<()>) -> std::io::Result<FocusServer> {
    let path = private_directory(directory)?.join("settings.sock");
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        if !meta.file_type().is_socket() || meta.uid() != unsafe { libc::geteuid() } {
            return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        }
        if UnixStream::connect(&path).is_ok() {
            return Err(std::io::Error::from(std::io::ErrorKind::AddrInUse));
        }
        std::fs::remove_file(&path)?;
    }
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    let stop = Arc::new(AtomicBool::new(false));
    let stopped = stop.clone();
    let thread = std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stopped.load(Ordering::Acquire) {
                break;
            }
            let Ok(mut stream) = stream else {
                break;
            };
            if !same_user(&stream) {
                continue;
            }
            if stream
                .set_read_timeout(Some(Duration::from_millis(500)))
                .is_err()
            {
                continue;
            }
            let mut bytes = [0; 5];
            let read = stream.read_exact(&mut bytes);
            if read.is_ok() && &bytes == b"OPEN\n" {
                let _ = tx.try_send(());
                let _ = stream.write_all(b"OK");
            }
        }
    });
    Ok(FocusServer {
        path,
        stop,
        thread: Some(thread),
    })
}
#[cfg(unix)]
pub fn open_existing(directory: &Path) -> std::io::Result<()> {
    let path = private_directory(directory)?.join("settings.sock");
    let meta = std::fs::symlink_metadata(&path)?;
    if !meta.file_type().is_socket()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o077 != 0
    {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    let mut stream = UnixStream::connect(path)?;
    if !same_user(&stream) {
        return Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
    }
    stream.set_read_timeout(Some(Duration::from_secs(2)))?;
    stream.write_all(b"OPEN\n")?;
    let mut ack = [0; 2];
    stream.read_exact(&mut ack)?;
    if &ack != b"OK" {
        return Err(std::io::Error::from(std::io::ErrorKind::InvalidData));
    }
    Ok(())
}
#[cfg(unix)]
impl Drop for FocusServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = UnixStream::connect(&self.path);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        let _ = std::fs::remove_file(&self.path);
    }
}
#[cfg(not(unix))]
pub struct FocusServer;

#[cfg(not(unix))]
impl Drop for FocusServer {
    fn drop(&mut self) {}
}

#[cfg(not(unix))]
pub fn serve(_directory: &Path, _tx: async_channel::Sender<()>) -> std::io::Result<FocusServer> {
    Ok(FocusServer)
}

#[cfg(not(unix))]
pub fn open_existing(_directory: &Path) -> std::io::Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    #[test]
    fn unrelated_file_at_socket_path_is_never_removed() {
        let directory = tempfile::tempdir().unwrap();
        let path = super::private_directory(directory.path())
            .unwrap()
            .join("settings.sock");
        std::fs::write(&path, b"keep").unwrap();
        let (tx, _) = async_channel::bounded(1);
        assert!(super::serve(directory.path(), tx).is_err());
        assert_eq!(std::fs::read(path).unwrap(), b"keep");
    }
    #[test]
    fn same_user_can_only_forward_the_open_settings_command() {
        let directory = tempfile::tempdir().unwrap();
        let (tx, rx) = async_channel::bounded(1);
        let server = super::serve(directory.path(), tx).unwrap();
        super::open_existing(directory.path()).unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while rx.try_recv().is_err() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        drop(server);
    }
}
