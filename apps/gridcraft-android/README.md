# GridCraft for Android

GridCraft on Android tablets and phones: the same app as on the desktop (the ribbon, formulas,
charts, `.xlsx` in and out), running natively. It is not a web page in a wrapper.

> **Community port, early preview.** GridCraft itself is still in development. Keep copies of
> workbooks that matter.

## Install

Open `GridCraft-<version>-arm64.apk` on the tablet and allow the install when Android asks.
Needs Android 8.0 or newer on a 64-bit ARM device (nearly every tablet and phone from the last
several years).

## Using it with fingers

| Gesture | Does |
|---|---|
| Tap | Click: select a cell, press a button |
| Double tap a cell | Edit it (the on-screen keyboard appears) |
| Swipe | Scroll the sheet (or a list, or a gallery); a flick keeps coasting |
| Long press, then drag | Select a range |
| Two fingers apart / together | Zoom |
| Two-finger tap | Right-click (the context menu) |
| Back button | Escape: closes a menu, a dialog or File view, or cancels an edit |

The on-screen keyboard appears when you double-tap a cell or tap the formula bar or a text box,
and goes away when nothing takes typing; the app keeps clear of it (and of the status and
navigation bars). A hardware keyboard works as on the desktop: type into the selected cell,
arrows, Tab, Enter, and shortcuts (Ctrl+S, Ctrl+B, Ctrl+Z, …).

## Files

- **Open** (File › Open, or Insert › Pictures) shows Android's file picker: device storage,
  an SD card, Google Drive and other installed storage apps. **Open with GridCraft** from a file
  manager or a mail attachment, and another app's **Share**, also work.
- **Save** (Ctrl+S, File › Save, AutoSave) of an `.xlsx`, `.xlsm`, `.csv` or `.tsv` opened from
  the picker writes back to that file. GridCraft edits a private working copy and copies it over
  the original after each save, but never over an original another app changed meanwhile: then
  it says so, and **Save As** keeps your changes as a new file.
- A new workbook, a shared file, an `.xls`/`.ods` (imported) and **Save As** ask where with
  Android's save picker; after Save As the workbook takes the name and file you chose.
  **Export** (PDF, CSV, web page) also asks where; the workbook keeps its own file.
- **Print** uses Android's print dialog (Wi-Fi printers, Save as PDF, cloud print services).
- **Copy and Paste** share the system clipboard with other apps, as a table where the other app
  understands one (so formatting comes along) and as text.
- Settings stay in the app's private storage. File › Open doesn't list recent workbooks:
  Android gives the app a file's contents, not a path it could open again. The system picker
  has its own Recent list.

## Building

`build-apk.sh` builds the APK with the Android SDK's own tools (no Gradle):

```sh
rustup target add aarch64-linux-android
export ANDROID_HOME=~/Android/Sdk          # with build-tools, platforms;android-35 and an NDK
apps/gridcraft-android/build-apk.sh        # -> dist/android/GridCraft-<version>-arm64.apk
adb install -r dist/android/GridCraft-*-arm64.apk
adb logcat -s GridCraft                    # the app's log
```

The first build creates a signing key in `apps/gridcraft-android/keys/` (not committed). Keep
it: Android installs an update only over a build signed with the same key. `APP_ID` changes the
package name (default `gridcraft.android`).

## How it fits together

| Piece | Job |
|---|---|
| `src/lib.rs` | `android_main`, the entry point NativeActivity calls |
| `src/shell.rs` | storage folders, `Services` (open, save, print, clipboard, links), preferences, the keyboard |
| `src/workcopies.rs` | working copies of workbooks opened from storage, and writing them back (tested on the desktop) |
| `src/touch.rs` | finger gestures → clicks, scrolling and drag-select (tested on the desktop) |
| `src/keys.rs` | on-screen keyboard keys → egui keys and modifiers (tested on the desktop) |
| `src/mailbox.rs` | the inbox the Java side answers through, in order (tested on the desktop) |
| `src/bridge.rs` | JNI calls into the activity (the only `unsafe` in the port) |
| `android/java/…/GridCraftActivity.java` | `NativeActivity` subclass: pickers, saving, writing back, printing, clipboard, the keyboard's text field, window insets |
| `winit-android-modifiers.patch` | applied to winit for the Android build only: reports Shift/Ctrl/Alt, which winit 0.30 never does on Android |
| `android/AndroidManifest.xml` | the app, its launcher entry and the file types it opens |
