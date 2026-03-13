use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::NSString;
use std::ptr::NonNull;

/// Apply dark appearance to the NSWindow backing a winit view.
/// This makes the native title bar and tab bar match the Solarized Dark
/// theme used by the terminal content area.
///
/// # Safety
/// `ns_view_ptr` must point to a valid NSView that belongs to an NSWindow.
pub unsafe fn apply_dark_titlebar(ns_view_ptr: &NonNull<std::ffi::c_void>) {
    let ns_view: &AnyObject = &*(ns_view_ptr.as_ptr() as *const AnyObject);
    let ns_window: *const AnyObject = msg_send![ns_view, window];
    if ns_window.is_null() {
        return;
    }
    let ns_window = &*ns_window;

    // Set standard dark appearance so title bar + tab bar render dark.
    // DarkAqua gives the standard dark gray; VibrantDark was too black.
    let appearance_name = NSString::from_str("NSAppearanceNameDarkAqua");
    let Some(appearance_class) = AnyClass::get(c"NSAppearance") else {
        return;
    };
    let appearance: *const AnyObject =
        msg_send![appearance_class, appearanceNamed: &*appearance_name];
    if !appearance.is_null() {
        let _: () = msg_send![ns_window, setAppearance: appearance];
    }

    // Set the window background to Solarized Dark base03 (#002b36).
    // With a non-transparent titlebar, this gives the titlebar a natural
    // dark bottom edge that separates it from the content.
    let Some(ns_color_class) = AnyClass::get(c"NSColor") else {
        return;
    };
    let bg: *const AnyObject = msg_send![
        ns_color_class,
        colorWithSRGBRed: 0.0_f64,
        green: 0.169_f64,
        blue: 0.212_f64,
        alpha: 1.0_f64
    ];
    if !bg.is_null() {
        let _: () = msg_send![ns_window, setBackgroundColor: bg];
    }
}
