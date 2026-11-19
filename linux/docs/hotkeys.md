# KDE Wayland hotkey spike — 2026-10-11

Chosen backend: **KDE kglobalaccel D-Bus**, with `globalShortcutPressed` and
`globalShortcutReleased` on `org.kde.kglobalaccel.Component`. Neither root nor
membership in `input` is required.
No evdev input reader or RemoteDesktop fallback was needed. `evdev` is used only
for writing a persistent uinput keyboard, which has a session ACL.

## Portal first

Tested ashpd 0.13.13 against the host portal, GlobalShortcuts version 2, Registry
version 1. Installed `cz.hajadlo.hark.desktop` on the desktop with `Name=Hark`.
An entry with `Exec=hark run` failed before hark was installed/on PATH:

```text
Could not register app ID: App info not found for 'cz.hajadlo.hark'
```

Pointing Exec at the built binary made Registry.Register(`cz.hajadlo.hark`, {})
and CreateSession succeed. The spike ran as
`app-cz.hajadlo.hark-spike2.service` through the graphical user manager.
BindShortcuts with preferred trigger `CTRL+ALT+SHIFT+F10` stayed pending.
Read-only AT-SPI inspection of the portal's own window found:

```text
frame / heading: Global Shortcuts Requested
label: Hark wants to register the following shortcut:
shortcut button: Ctrl+Alt+Shift+F10
buttons: OK, Cancel, See Other Shortcuts…
```

No button was clicked. Stopping the transient spike closed its session/dialog.
Portal release delivery remains unverified; this is a confirmation gate, not
evidence that KDE's portal lacks release. You can run
`hark portal-spike` as `app-cz.hajadlo.hark-spike.service`, accept OK, and repeat
with the fixed test chord if portal acceptance is desired. The shipped backend
needs no portal dialog. [ashpd's API](https://docs.rs/ashpd/0.13.13/ashpd/desktop/global_shortcuts/struct.GlobalShortcuts.html)
and [KDE's implementation](https://github.com/KDE/xdg-desktop-portal-kde/blob/master/src/globalshortcuts.cpp)
document the relevant interfaces.

## KDE fallback evidence

Confirmed the test chord was absent from kglobalshortcutsrc and available through
isGlobalShortcutAvailable before registration. Qt key code is `0x0f000039`
(decimal 251658297). Registered a four-string action id and used setShortcut
flags `6 = SetPresent | NoAutoloading`; `2` alone autoloaded an empty saved key.
Verified the returned binding exactly matched the requested key before injection.
The [KDE flag definitions](https://github.com/KDE/kglobalaccel/blob/master/src/kglobalaccel_p.h)
and [component signals](https://github.com/KDE/kglobalacceld/blob/master/src/component.h)
are the primary references. No existing shortcut was stolen.

Initial ydotool hold, October 11 at 21:06 Prague time:

```text
BOUND Ctrl+Alt+Shift+F10 key=251658297
21:06:56 globalShortcutPressed ("cz.hajadlo.hark", "test", 0)
21:06:56–57 globalShortcutRepeated ("cz.hajadlo.hark", "test", 0)
21:06:57 globalShortcutReleased ("cz.hajadlo.hark", "test", 0)
```

KDE's D-Bus timestamp is **zero** on this installation. Hark timestamps receipt
with `Instant`; timing does not use the KDE timestamp. Repeated signals are
ignored. The retained `hotkey-test-desktop.sh` independently queries binding
ownership, injects only the fixed test chord with hark's own keyboard, and
asserts both edges and an 800–2000 ms interval:

```text
BOUND Ctrl+Alt+Shift+F10 → raw
1608 ms Pressed Ctrl+Alt+Shift+F10
2607 ms Released Ctrl+Alt+Shift+F10
PASS press/release interval: 999 ms
```

A five-key virtual device was not classified as a keyboard by udev. Advertising
ordinary keyboard keys gives `ID_INPUT_KEYBOARD=1`; a 250 ms startup wait still
lost the first chord. With a 1500 ms discovery wait the probe passes. Production
creates the device once before binding shortcuts and retains it. No Ctrl+V was
sent during development.

Hark subscribes to `NameOwnerChanged` for `org.kde.kglobalaccel` before binding.
Loss or replacement of the owner returns an error and exits with status 1;
the user unit's `Restart=on-failure` binds again after 3 seconds. Its
`StartLimitIntervalSec=0` prevents a KDE outage from exhausting restart attempts.
Registration and cleanup target the original unique owner, so cleanup cannot
remove a replacement service's bindings. A unit test exercises both owner loss
and replacement on a private D-Bus session; the user's live KDE service stays
untouched.

## Ctrl+Space takeover

Read-only inspection confirmed Handy's current mappings and insertion settings:

- Ctrl+Space: transcribe_with_post_process.
- Ctrl+Shift+Space: transcribe.
- ctrl_v, dont_modify, paste_delay_ms=60.
- Autostart: `app-Handy@autostart.service`, from the existing Handy.desktop.

KDE's `action` lookup returns an empty array for both chords while Handy is
running. It cannot prove that Handy has released its own shortcut backend.
Hark additionally checks for a running `handy` process when these two chords
are configured, and refuses to bind until it exits. This is a guard against
known Handy contention, not a general detector for every non-KDE application.

At acceptance: quit Handy through its tray UI, disable its startup in KDE's
Autostart settings while keeping the installation, then run hark check and
start app-cz.hajadlo.hark.service. Defaults match Handy's current meanings.
Test hold, tap/tap and raw insertion in an explicitly chosen text field.
Enable the hark user unit only after those checks. For rollback stop/disable
hark and start Handy/re-enable its autostart. No input-group step is needed.
