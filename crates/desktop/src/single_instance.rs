use std::{
    fs::{File, OpenOptions, TryLockError},
    io,
    path::Path,
};

pub struct SingleInstance {
    _file: File,
}

impl SingleInstance {
    pub fn acquire(path: &Path) -> io::Result<Option<Self>> {
        let mut options = OpenOptions::new();
        options.create(true).truncate(false).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Some(Self { _file: file })),
            Err(TryLockError::WouldBlock) => Ok(None),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_instance_is_rejected_until_owner_exits() {
        let path =
            std::env::temp_dir().join(format!("vpn-splitter-lock-test-{}", std::process::id()));
        let first = SingleInstance::acquire(&path).unwrap().unwrap();
        assert!(
            SingleInstance::acquire(&path).unwrap().is_none(),
            "a second process must not acquire control"
        );
        drop(first);
        assert!(
            SingleInstance::acquire(&path).unwrap().is_some(),
            "the OS releases the lock when the owner exits"
        );
        std::fs::remove_file(path).unwrap();
    }
}
