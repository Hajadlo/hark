# Hark for KDE Wayland

One Rust binary for the desktop and laptop. The default shortcuts mirror Handy's
current setup: Ctrl+Space cleans dictation; Ctrl+Shift+Space returns raw text.
A press begins capture immediately. Release before 300 ms keeps recording until
any configured shortcut is pressed again; a longer hold stops on release.
Capture stops at ten minutes. Busy presses are ignored.

The host portal required confirmation, so the client uses KDE's kglobalaccel
D-Bus press/release signals. See `docs/hotkeys.md` for the spike, event logs,
exact dialog, and takeover procedure. No input-group membership is needed.

## Build and checks

Needs Rust and the ALSA, D-Bus and Wayland headers (on Fedora:
`alsa-lib-devel pkgconf-pkg-config gcc dbus-daemon`).

```sh
cd linux
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release
```

## Install and acceptance

```sh
cd linux
scripts/install.sh target/release/hark
```

This installs the binary, desktop entry, and `app-cz.hajadlo.hark.service`, reloads
the user manager, and creates a mode-600 config template if none exists. It does
not start or enable anything. Edit the private config locally; do not put the
token in a command argument, shell history, or a screenshot.

1. Set the real base URL/token and device `desktop` in
   `~/.config/hark/config.toml`; keep `insert="paste"`. Omit `input_device` to
   follow the current system default source on each recording.
2. Quit Handy through its tray UI. Turn off its own "Launch at startup"
   setting (`autostart_enabled` in Handy's settings store), then disable it in
   KDE's Autostart settings; with the setting on, Handy re-creates its autostart
   entry at the next launch. Keep Handy installed for rollback. The daemon waits while Handy is running, then
   binds the default chords when it exits; KDE alone cannot see its reservations.
3. Run `~/.local/bin/hark check`, then
   `systemctl --user start app-cz.hajadlo.hark.service`.
4. Focus an explicitly chosen scratch text field. Hold Ctrl+Space and speak,
   then test tap/tap and Ctrl+Shift+Space. Confirm cleaned/raw insertion, short
   sounds, and restoration of an existing clipboard value.
5. Test a failed upload and the KDE Retry/Discard actions with a test config and
   synthetic speech, then restore the real config/restart the service.
6. Enable with `systemctl --user enable app-cz.hajadlo.hark.service` after
   successful acceptance.
7. On a second machine, install the same binary, set another device name and
   repeat steps 1–6.

Rollback: stop and disable app-cz.hajadlo.hark.service, start Handy, and turn
its "Launch at startup" setting back on. Development changed neither Handy nor its startup.

## Configuration and CLI

`config.example.toml` shows every setting. Shortcuts are an array of key/mode
pairs. Supported keys: Space, Escape, F1–F35, letters and digits, with Ctrl,
Shift, Alt or Meta/Super modifiers. At least one modifier is required.
For dictation, avoid shortcuts containing Alt or Meta: if either remains held
when insertion fires, it changes the injected Ctrl+V. Ctrl and Shift are
harmless; Ctrl+Shift+V also pastes in Electron apps and terminals.
`input_device` is an exact cpal/ALSA device name from `hark audio-devices`.
CPAL 0.16 lists built-in ALSA PCMs and available hardware, not arbitrary ALSA
hint names. For a specific PipeWire source, a process-specific ALSA config can
point the `pipewire` PCM at that node; the E2E script shows a working isolated
example. Do not change global ALSA/default-device settings for tests.

If recording starts but always returns "Nothing heard", inspect the actual
PipeWire link for `alsa_capture.hark`. EasyEffects can redirect that stream to
`easyeffects_source` even when the system default is an unmuted physical mic.
If the virtual source is muted, Hark receives silence. Excluding
`alsa_capture.hark` in EasyEffects' input-stream blocklist lets Hark follow the
normal microphone without unmuting the virtual mic for other applications.
The desktop uses this exclusion; physical/default devices and their volumes
remain unchanged. Confirm the actual source link again after a fresh recording,
then check speech through the normal shortcut.

```sh
hark run
hark transcribe synthetic.wav --mode raw
hark check
hark --config private-test-config.toml run
```

One-shot files are decoded in memory and converted to the same 16 kHz mono
16-bit PCM wire format. Files over ten minutes are rejected. The daemon captures
through cpal's ALSA/PipeWire plugin, downmixes and resamples in memory, and caps
encoded audio to ten minutes. No daemon recording is written to disk.

`overlay = true` (the default) shows a quiet floating recording chip near the
bottom centre of the main screen, above KDE's panel. It is fully click-through,
uses no keyboard focus, and disappears when idle. The red dot and small meter
follow smoothed mic RMS; the mode label stays visible, and toggle recording adds
“tap to stop”. Uploading and pasting show “Processing…”, then fade out over
150 ms. “Nothing heard” and “Failed” remain for 1.5 seconds before fading.

Omit `overlay_output` to follow the first connector in KDE's output order.
`overlay_output = "DP-4"` pins a connected output; if it is unavailable, Hark
falls back to KDE's order, then compositor choice. Output changes re-evaluate
the selection, including hot-plug and laptop lid changes. Rendering uses the
ceiling integer buffer scale for crisp fractional scaling. Text uses the system
sans font found through fontconfig; without it the indicator still draws.
Set `overlay = false` to disable it entirely. The overlay has its own Wayland
connection and thread, sleeps without a timer while hidden, and cannot stop
dictation if Wayland or layer-shell is unavailable. After a disconnected or
failed worker, the next state change starts a fresh connection, with at most
one creation attempt every five seconds. There is no retry timer while idle;
a missing compositor logs once until a connection recovers.

For review without viewing the desktop, run `hark overlay-previews <directory>`.
It renders the same drawing code into 28 PNGs: seven recording/processing/message
variants, at scales 1.0 and 1.25, on light and dark checkered backgrounds.

Implementation references: [SCTK layer-shell example](https://github.com/Smithay/client-toolkit/blob/v0.20.0/examples/simple_layer.rs)
and [KDE output-order bindings](https://docs.rs/wayland-protocols-plasma/0.3.12/wayland_protocols_plasma/output_order/v1/client/kde_output_order_v1/index.html).

Multipart uploads carry a known Content-Length, Bearer authentication,
X-Hark-Device and hark-linux/version. The read timeout and total request timeout
are 120 seconds, including body writes. All attempts and backoff also share a
120-second deadline. There are up to three attempts with 1- and 2-second waits
for network errors/timeouts and 5xx.

After the last failed attempt, shortcuts are ready immediately. Audio from
network/5xx failures stays in memory until Retry succeeds, Discard is chosen,
or a new recording starts. Terminal HTTP rejections (including all 4xx) discard
the audio and offer no Retry. Recordings shorter than 200 ms stay local and show
“Nothing heard”. Dismissing the notification leaves that state unchanged. Retry
during recording, upload or insertion gives the busy sound. Successful empty
text gives a short signal and no insertion.

Insertion runs outside the event loop, keeps gestures busy, and has a two-second
overall deadline. The paste phase ends early to reserve the configured restore
delay plus 500 ms for clipboard restoration and a small scheduling margin.
Cleanup still runs if copying fails or the paste phase expires after ownership
changes. A cancelled or expired paste phase never sends a late Ctrl+V.
Clipboard saves have 250 ms per MIME type and 500 ms in total;
a stalled type is skipped. If no backup is readable, dictation is still pasted
with a warning and clipboard restoration is skipped. `wl-copy` and the restore
worker handshake each have 500 ms. The restore owner handles concurrent and
repeated reads and exits on ownership loss. Error notifications coalesce to the
latest error at most once every five seconds; notification failures are logged
and cannot stop dictation. Stopping the daemon discards pending audio and closes
its notification.

`check` requests `/v1/models` with the required X-Hark-Web header. That route
also requires owner identity from Tailscale Serve under the new contract; it
cannot succeed against an owner-enforcing bare loopback gateway with Bearer
authentication alone. The private E2E gateway here is the baseline server from
this branch. Nothing was changed in the shared API file.

Pasting saves every MIME type it can read within the budget before changing
clipboard ownership. It uses wl-copy, waits 60 ms, emits
Ctrl+V through a persistent uinput keyboard, waits `restore_delay_ms` (default
60 ms, adjustable from 0 to 1000 ms for slower applications) and restores
all captured types. A separate owner process keeps the restored clipboard
available even after daemon exit; it exits when clipboard ownership changes.
The user unit uses KillMode=process for that reason. uinput failure is reported
at startup; RemoteDesktop/libei is not implemented because the tested ACL works.

The daemon watches kglobalaccel's D-Bus owner and exits with a clear error on
loss or replacement. `Restart=on-failure` re-registers configured shortcuts;
`StartLimitIntervalSec=0` allows repeated attempts while KDE is restarting.

Tests with `device = "test"`, `insert = "print"`, and the fixed
Ctrl+Alt+Shift+F10 chord use a separate `cz.hajadlo.hark.Test` D-Bus component,
so the installed daemon can keep its real shortcuts. The desktop harness
verifies DP-4 mapping, processing, fade/unmap and an idle CPU sample. Set
`HARK_TEST_NO_WAYLAND=1` to test dictation without a usable Wayland connection.
`HARK_TEST_TOGGLE_ONLY=1` skips the hold check when another running process is
creating/removing keyboards and causing premature KWin release events; that
variant is reported explicitly and does not replace a successful full hold test.

