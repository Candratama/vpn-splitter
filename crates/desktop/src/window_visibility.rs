use gpui::Window;
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[cfg(target_os = "macos")]
fn native_window(window: &Window) -> Result<objc2::rc::Retained<objc2_app_kit::NSWindow>> {
    let handle = HasWindowHandle::window_handle(window)?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("Expected an AppKit window".into());
    };
    // GPUI owns this view; the borrowed handle is valid for this Window on
    // the GPUI main thread. Retaining its NSWindow outlives the handle borrow.
    let view = unsafe { &*handle.ns_view.as_ptr().cast::<objc2_app_kit::NSView>() };
    view.window()
        .ok_or_else(|| "Settings has no native window".into())
}

#[cfg(target_os = "windows")]
fn native_window(window: &Window) -> Result<windows::Win32::Foundation::HWND> {
    let handle = HasWindowHandle::window_handle(window)?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err("Expected a Win32 window".into());
    };
    Ok(windows::Win32::Foundation::HWND(
        handle.hwnd.get() as *mut std::ffi::c_void
    ))
}

pub fn hide(window: &Window) -> Result<()> {
    let window = native_window(window)?;
    #[cfg(target_os = "macos")]
    window.orderOut(None);
    #[cfg(target_os = "windows")]
    unsafe {
        let _ = windows::Win32::UI::WindowsAndMessaging::ShowWindow(
            window,
            windows::Win32::UI::WindowsAndMessaging::SW_HIDE,
        );
    }
    Ok(())
}

pub fn is_visible(window: &Window) -> Result<bool> {
    let window = native_window(window)?;
    #[cfg(target_os = "macos")]
    {
        Ok(window.isVisible())
    }
    #[cfg(target_os = "windows")]
    unsafe {
        Ok(windows::Win32::UI::WindowsAndMessaging::IsWindowVisible(window).as_bool())
    }
}
