//! GridCraft on Android phones and tablets.
//!
//! The same [`gridcraft_ui_egui::SheetApp`] as the desktop and web apps, run by eframe's
//! NativeActivity runner (wgpu: Vulkan, or OpenGL ES where Vulkan is missing). Android loads this
//! library (`libgridcraft_android.so`) into `GridCraftActivity`, a small `NativeActivity` subclass
//! (`android/java/…`) that adds what NDK code can't do alone: the system file pickers, sharing the
//! clipboard, printing, links and the on-screen keyboard's text.
//!
//! Differences from the desktop app, much like the browser's (`apps/gridcraft-web`):
//! - File › Open and Insert › Pictures use Android's document picker; picked files arrive through
//!   the app's inbox ([`mailbox`]). Opening a workbook from a file manager or another app's
//!   Share/Open with does the same.
//! - A workbook opened from storage that GridCraft may write back to is edited in a private
//!   working copy ([`workcopies`]): Save and AutoSave write the copy, and the shell then copies it
//!   over the original, never over one that changed elsewhere meanwhile. Save As and exports go
//!   through Android's save picker; after Save As the workbook takes the new file.
//! - The on-screen keyboard types into the activity's own text field, which passes text and keys
//!   through the inbox in order ([`keys`]); the app keeps clear of the status bar, navigation bar
//!   and keyboard.
//! - Fingers: tap clicks, swipe scrolls, long press then drag selects, two fingers pinch-zoom,
//!   and a two-finger tap opens the context menu ([`touch`]). The system Back button is Escape.
//! - Settings live in the app's private storage.
//!
//! Build the APK with `apps/gridcraft-android/build-apk.sh` (see the README beside it).
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

pub mod keys;
pub mod mailbox;
pub mod touch;
pub mod workcopies;

#[cfg(target_os = "android")]
mod bridge;
#[cfg(target_os = "android")]
mod shell;

/// The entry point android-activity's glue calls on its own thread once the activity starts.
// SAFETY: `android_main` must be exported unmangled: the android-activity glue looks it up by
// this exact name. Nothing else in the process defines it.
#[cfg(target_os = "android")]
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
fn android_main(app: winit::platform::android::activity::AndroidApp) {
    shell::run(app);
}
