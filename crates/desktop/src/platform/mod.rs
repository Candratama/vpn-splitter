#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
#[allow(unused_imports)]
pub use macos::Profile;

#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "windows")]
#[allow(unused_imports)]
pub use windows::Profile;
