# hark Android

The framework Views app is `cz.hajadlo.hark`, minimum Android 13 (API 33).
SwiftKey remains the typing keyboard. Voice input can start from its microphone
button through Android's speech-recognition activity, or through hark's auxiliary
voice keyboard. Both show a temporary recording panel, upload to the configured
server, and return the result to the calling keyboard/editor. Standard Android
result delivery does not need accessibility. The optional direct-insertion mode
uses hark Accessibility to bypass keyboards that lose their editor connection.
Neither route uses the clipboard. The floating microphone is independently optional.

The 0.2.1 recording panel adds a live microphone level display. Its bars use
the captured samples, so silence stays flat and speech raises the bars. The
display stops when recording stops; transcription still happens on the server.

Version 0.2.2 adds **Insert keyboard dictation directly** and **Show floating
microphone** as independent settings. Direct insertion pins the original editor,
waits for it to become active again, restores its focus if needed, and replaces
only the current selection. Another focused editor, a changed window, a stale
node, or a nonempty draft without valid cursor offsets prevents insertion.
Android reports empty hint-only editors with selection offsets `-1:-1`; these
are safely treated as insertion at zero after discarding the displayed hint.

## Keyboard microphone setup (0.2.2)

This build adds two Android entry points because keyboards use different voice
handoffs: `android.speech.action.RECOGNIZE_SPEECH` and an input method with a
`voice` subtype. [FUTO's compatibility notes](https://github.com/futo-org/voice-input#keyboard-support)
identify SwiftKey as using the first. The Pixel's installed SwiftKey uses that
route when Google voice typing is disabled, as described below.

1. Update the installed app, preserving its package/signing key so the existing
   server URL and token remain. Open hark and confirm microphone permission.
   Keep Tailscale connected.
2. In **SwiftKey → Rich input**, turn **Multi-modal voice typing** off. Microsoft
   documents this as the switch out of SwiftKey's built-in dictation:
   [voice typing options](https://support.microsoft.com/en-us/swiftkey-keyboard/how-do-i-use-voice-to-text-with-microsoft-swiftkey-keyboard).
3. From hark's **Keyboard settings** button, enable **hark voice input** alongside
   SwiftKey. Keep SwiftKey as the typing keyboard. On the Pixel's installed
   SwiftKey build, also disable **Google voice typing** in that keyboard list:
   when it is enabled SwiftKey opens Google's voice IME; when disabled it uses
   the speech-intent chooser. Leave the Google speech/TTS application installed.
4. Tap SwiftKey's microphone (or hold its comma key). If Android offers an app
   chooser, choose **hark voice input**, then **Always**. If SwiftKey still opens Google voice
   input, verify its installed version and voice-input selection; this is not a
   custom-server URL setting in SwiftKey.
5. For this Pixel's SwiftKey/T3 combination, enable **Insert keyboard dictation
   directly**, disable **Show floating microphone**, and enable **hark** in
   Android **Accessibility**. The service inserts into the original field
   without showing a bubble. With direct insertion disabled, Android's standard
   result-return path remains available without accessibility access.

The panel starts recording when opened. Tap **Stop and transcribe** to upload;
the text returns to SwiftKey, direct insertion writes to the original editor,
or the voice keyboard commits to its active editor. Direct insertion returns
`RESULT_CANCELED` to SwiftKey so the keyboard cannot insert a second copy.
**Cancel**, leaving the
activity, hiding the voice keyboard, or changing editors dismisses the request;
late results cannot type through a closed panel. An upload already in progress
can still finish and appear in server History. Failed audio keeps the existing
pending-recording/notification recovery path in hark settings. Retrying there
uses the existing accessibility insertion path, so with accessibility disabled,
read the recovered transcript in History.

On the Pixel 8a / Android 17, the installed SwiftKey opens its own
`VoiceInputHelperActivity`, which replaces the editor's input connection before
hark opens. The user confirmed Discord only inserted after another tap, and
T3 did not insert even after refocus. A controlled fixed-transcript test
reproduced the empty T3 result. Changing hark's window focus flags did not fix
it. Direct insertion now passes that same device check **before any refocus
tap**, both in an empty T3 composer and when replacing selected synthetic text
while preserving surrounding draft text. The tests remove their unsent fixture
and restore and byte-verify the original server preferences afterward.

The user then verified real speech through SwiftKey's mic in **both T3 Code
and Discord**, with text appearing automatically after Stop and no refocus tap.
The installed clean 0.2.2 APK was checksum-verified, normal server preferences
restored, direct insertion enabled, floating microphone disabled, and SwiftKey
kept as the typing keyboard. Temporary diagnostics and the test endpoint were
removed; the original USB screen-awake preference was restored.

Czech, cancel/background during upload, and intentionally enabled bubble
behavior remain separate acceptance cases. Selecting the auxiliary voice IME directly
hit a microphone foreground-service restriction in the ADB check; it is not an
accepted fallback on this phone. SwiftKey's own "Try out your setup" field
disappears when recognition opens; use an ordinary editor for acceptance.
Automated
Robolectric checks cover Android result return, panel stop/cancel, permission
denial, duplicate/late results, and voice-IME discovery; they do not verify the
installed SwiftKey build or real microphone behavior.

Do not run plain `adb shell uiautomator dump` between opening the recording panel
and completing direct insertion: UIAutomation suppresses accessibility services
by default and invalidates the pinned target. Use screenshots and `dumpsys`
during the handoff, or instrumentation with
[`FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES`](https://developer.android.com/reference/android/app/UiAutomation#FLAG_DONT_SUPPRESS_ACCESSIBILITY_SERVICES).
Inspect the resulting field only after delivery completes. Restore normal
server settings before asking the user to speak.

## Build

`android/Containerfile` holds the whole toolchain (Temurin JDK 17, SDK
platform 37, AGP 9.4 with built-in Kotlin, Gradle 9.6 wrapper), so no host
Java is needed:

```sh
podman build -t hark-android android
podman run --rm -v "$PWD/android:/src:Z" -v "$HOME/.android:/signing:Z" -w /src \
  hark-android ./gradlew testDebugUnitTest assembleDebug
adb install -r -g android/app/build/outputs/apk/debug/app-debug.apk
```

The debug build signs with `/signing/android-debug.keystore` (or
`HARK_DEBUG_KEYSTORE`); keep the same key so updates install over the old app.
Runtime code uses only Android's framework, the Kotlin standard library and
AndroidX core; JUnit, org.json and Robolectric are test-only.

## Optional bubble setup

1. Ensure Tailscale is connected and Android Advanced Protection is off.
   Advanced Protection can revoke this app's accessibility access because
   it is not declared an accessibility tool.
2. Open hark. Use App info → ⋮ → Allow restricted settings if that option
   exists. It may be unnecessary after an adb install; check on the phone.
   If the accessibility switch remains blocked, you can try
   the AOSP app-op idea below on the desktop. It is an experiment, not a
   guaranteed substitute for the UI step:

   ```sh
   "$HOME/.local/share/android-tools/platform-tools/adb" shell appops set cz.hajadlo.hark ACCESS_RESTRICTED_SETTINGS allow
   ```

3. Grant microphone and notification permissions with the button in hark.
   Notification access makes failed-recording Retry/Discard actions visible.
   Successful dictations have no result notification or “Inserted” toast.
   The mandatory recording indicator uses a separate minimum-importance
   channel with no sound, vibration, badge or actions. Its display is deferred
   so short sessions (about ten seconds or less) normally finish before it
   appears; Android controls the exact delay. Longer sessions can show a quiet
   indicator. The old `dictation` channel is deleted when the service starts.
   Failures use the separate “Failed dictations” channel at default importance;
   failed clips also remain in the pending list in settings.
   If previously denied permanently, use App info → Permissions.
4. Set the HTTPS server URL, bearer token and mode, then Save and Test.
   A pasted trailing `/v1/` is removed. Expect `OK — clean, raw` (plus any
   configured modes). Test loads the mode dropdown; typed names work offline.
   The token is a password field in private preferences; Android backup is
   disabled. Do not include it in screenshots, logs or bug reports.
5. Open Accessibility settings and enable hark. Keep SwiftKey as the keyboard.

## Acceptance on Pixel 8a / Android 17

1. In T3 Code, focus the composer and show SwiftKey. Expect a translucent
   blue-grey bubble with a white microphone glyph. It should disappear when
   neither a keyboard nor editable focus is present, and when the screen
   turns off. A recording or upload keeps it visible while the screen is on.
2. Hold the bubble for at least 300 ms while saying synthetic text. Expect
   a solid red, gently pulsing bubble and Android's microphone privacy
   indicator, then a dark grey bubble with a turning white ring on release.
   The text should appear at the cursor, with no result notification or
   success toast. Short sessions should normally show no foreground-service
   notification; longer sessions can show the quiet recording indicator.
   Check existing text around the cursor, a selected range, and unchanged
   clipboard contents. Try T3 first, then another editor for comparison.
3. Tap briefly. Expect recording to continue after release; the next press
   stops it and uploads. Pressing while processing should only show `Busy`.
   The service starts before AudioRecord, but microphone-start eligibility
   from an accessibility overlay still needs checking on this actual phone.
4. Drag the idle bubble more than 24 dp. Expect the recording started by
   touch-down to cancel, no upload, and the saved position to survive service
   restart. Small movement should remain a tap/hold. Rotation should clamp
   the saved bubble within the screen.
5. Record with `raw`, then with `clean`. Change modes during a recording and
   verify the starting mode is used. A ten-minute recording should stop by
   itself; both the gesture clock and sample writer enforce the cap.
6. Use an invalid token to force a 401. Expect the server message plus
   “check the token”, a purple `!` bubble for about two seconds, and a failed-clip notification
   offering Retry and Discard. Restore the token, then retry. A network/5xx
   failure should make three attempts with 1 s and 2 s delays first.
7. Verify failed clips appear in settings, Discard removes them, and a
   successful retry removes them. Text is inserted where the cursor is when
   the result arrives, including after navigation during upload or retry.
   The input connection is tried first, even without a focused accessibility
   node; the focused node is needed only for the fallback. Expect
   a short “Transcribed; open History to read the text” toast only if neither
   insertion path could insert. The input-connection API returns void, so acceptance
   must confirm T3 actually inserts text; it cannot reliably report a silent
   commit failure back to hark.
8. After success or discard, confirm the matching WAV and sidecar are gone.
   Clips older than 24 hours are deleted when settings/service starts.
   App cache may also be reclaimed by Android; it is not permanent storage.
   Record silence to check the short `Nothing heard` toast and no insertion.
   The only other toast is `Busy`; upload errors use the bubble and failure
   notification.

## Tests and logs

Local JVM tests cover WAV bytes, multipart bytes/length, URL normalization,
response/error parsing, status retry decisions, the real transport against
a controlled HTTP server, and fake-clock gestures. The real gateway test is skipped unless `HARK_TEST_URL` is set.

Phone logs:

```sh
ADB=adb
"$ADB" logcat -v time --pid="$("$ADB" shell pidof cz.hajadlo.hark)"
# For platform service-start/permission failures (review before sharing):
"$ADB" logcat -v time ActivityManager:I AccessibilityManagerService:I AndroidRuntime:E '*:S'
```

The app does not log tokens, audio or transcript text. Android/system logs
may contain other apps' data; narrow by PID and inspect locally. A successful
200 deletes the WAV; failed clips remain in app cache until success, discard
or expiration. To inspect that cache in a debuggable install:

```sh
"$ADB" shell run-as cz.hajadlo.hark ls -l cache/recordings
```
