use anyhow::{Context, Result, ensure};
use evdev::{AttributeSet, EventType, InputEvent, KeyCode, uinput::VirtualDevice};
use rustix::{
    event::{PollFd, PollFlags, Timespec, poll},
    fs::{OFlags, fcntl_getfl, fcntl_setfl},
};
use std::{
    io::{Read, Write},
    os::fd::AsFd,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};
use wl_clipboard_rs::paste;
pub const INSERT_BUDGET: Duration = Duration::from_secs(2);
const SAVE_BUDGET: Duration = Duration::from_millis(500);
const MIME_BUDGET: Duration = Duration::from_millis(250);
const STEP_BUDGET: Duration = Duration::from_millis(500);
const RESTORE_BUDGET: Duration = STEP_BUDGET;
const RESTORE_MARGIN: Duration = Duration::from_millis(50);
const MAX_BYTES: usize = 64 * 1024 * 1024;
pub struct Keyboard {
    device: VirtualDevice,
}
impl Keyboard {
    pub fn new() -> Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        // udev's input_id needs ordinary alphanumeric capabilities to classify
        // this as a keyboard. Five shortcut keys alone are ignored by libinput.
        for code in 1..=255 {
            keys.insert(KeyCode::new(code));
        }
        let device = VirtualDevice::builder()?
            .name("Hark virtual keyboard")
            .with_keys(&keys)?
            .build()
            .context("uinput unavailable; check session ACL on /dev/uinput")?;
        // KWin discovery is asynchronous; 250 ms lost the first chord on this host.
        thread::sleep(Duration::from_millis(1500));
        Ok(Self { device })
    }
    fn event(&mut self, key: KeyCode, value: i32) -> Result<()> {
        self.device
            .emit(&[InputEvent::new(EventType::KEY.0, key.code(), value)])?;
        Ok(())
    }
    pub fn paste(&mut self, deadline: &Deadline) -> Result<()> {
        deadline.check()?;
        self.event(KeyCode::KEY_LEFTCTRL, 1)?;
        let result: Result<()> = (|| {
            deadline.check()?;
            self.event(KeyCode::KEY_V, 1)?;
            thread::sleep(Duration::from_millis(10));
            self.event(KeyCode::KEY_V, 0)
        })();
        let release = self.event(KeyCode::KEY_LEFTCTRL, 0);
        result?;
        release
    }
    // Only the fixed test chord; caller must verify it is bound before injection.
    pub fn test_chord(&mut self, hold: Duration) -> Result<()> {
        let keys = [
            KeyCode::KEY_LEFTCTRL,
            KeyCode::KEY_LEFTALT,
            KeyCode::KEY_LEFTSHIFT,
            KeyCode::KEY_F10,
        ];
        let result: Result<()> = (|| {
            for k in keys {
                self.event(k, 1)?;
            }
            thread::sleep(hold);
            Ok(())
        })();
        for k in keys.into_iter().rev() {
            let _ = self.event(k, 0);
        }
        result
    }
}

fn timeout() -> anyhow::Error {
    std::io::Error::new(std::io::ErrorKind::TimedOut, "step deadline exceeded").into()
}
fn check(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(timeout());
    }
    Ok(())
}
fn nonblocking(fd: &impl AsFd) -> Result<()> {
    fcntl_setfl(fd, fcntl_getfl(fd)? | OFlags::NONBLOCK)?;
    Ok(())
}
fn wait(fd: &impl AsFd, flags: PollFlags, deadline: Instant) -> Result<()> {
    loop {
        check(deadline)?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        let timeout = Timespec::try_from(remaining)?;
        let mut fds = [PollFd::new(fd, flags)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) => return Err(crate::insert::timeout()),
            Ok(_) => return Ok(()),
            Err(rustix::io::Errno::INTR) => continue,
            Err(e) => return Err(e.into()),
        }
    }
}
// Poll nonblocking pipes: a readable prefix does not imply the owner will send EOF.
pub(crate) fn read_pipe<R: Read + AsFd>(
    pipe: &mut R,
    bytes: &mut Vec<u8>,
    deadline: Instant,
) -> Result<()> {
    nonblocking(pipe)?;
    let mut buf = [0; 8192];
    loop {
        wait(pipe, PollFlags::IN, deadline)?;
        match pipe.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => {
                ensure!(
                    bytes.len() + n <= MAX_BYTES,
                    "clipboard type exceeds 64 MiB"
                );
                bytes.extend_from_slice(&buf[..n]);
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
}
fn write_pipe<W: Write + AsFd>(pipe: &mut W, mut bytes: &[u8], deadline: Instant) -> Result<()> {
    nonblocking(pipe)?;
    while !bytes.is_empty() {
        wait(pipe, PollFlags::OUT, deadline)?;
        match pipe.write(bytes) {
            Ok(0) => anyhow::bail!("clipboard pipe closed while writing"),
            Ok(n) => bytes = &bytes[n..],
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) => {}
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
fn warn_timeout<T>(step: &str, result: Result<T>) -> Result<T> {
    if result.as_ref().err().is_some_and(|e| {
        e.downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::TimedOut)
    }) {
        eprintln!("warning: {step} exceeded its time budget");
    }
    result.with_context(|| step.to_owned())
}
// Every helper is reaped on error/timeout. Killing a blocked Wayland roundtrip
// is possible here, unlike cancelling a spawn_blocking thread or library call.
struct Helper(Option<Child>);
impl Helper {
    fn spawn(command: &mut Command) -> Result<Self> {
        Ok(Self(Some(
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
        )))
    }
    fn child(&mut self) -> &mut Child {
        self.0.as_mut().unwrap()
    }
    fn finish(&mut self, deadline: Instant) -> Result<()> {
        loop {
            check(deadline)?;
            if let Some(status) = self.child().try_wait()? {
                ensure!(status.success(), "clipboard helper failed");
                self.0.take();
                return Ok(());
            }
            thread::sleep(Duration::from_millis(2));
        }
    }
    fn keep(mut self) -> u32 {
        let mut child = self.0.take().unwrap();
        let pid = child.id();
        thread::spawn(move || {
            let _ = child.wait();
        });
        pid
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}
fn helper_output(args: &[&str], deadline: Instant, step: &str) -> Result<Vec<u8>> {
    warn_timeout(
        step,
        (|| {
            check(deadline)?;
            let mut child = Helper::spawn(
                Command::new(std::env::current_exe()?)
                    .args(args)
                    .stderr(Stdio::null()),
            )?;
            child.child().stdin.take();
            let mut bytes = Vec::new();
            read_pipe(
                &mut child.child().stdout.take().context("helper stdout")?,
                &mut bytes,
                deadline,
            )?;
            child.finish(deadline)?;
            Ok(bytes)
        })(),
    )
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clipboard {
    sources: Vec<(String, Vec<u8>)>,
}
impl Clipboard {
    pub fn save() -> Result<Self> {
        Self::save_until(Instant::now() + SAVE_BUDGET, true)
    }
    fn save_until(deadline: Instant, strict: bool) -> Result<Self> {
        let deadline = deadline.min(Instant::now() + SAVE_BUDGET);
        let bytes = helper_output(
            &["clipboard-types"],
            deadline.min(Instant::now() + MIME_BUDGET),
            "clipboard MIME list",
        )?;
        let types: Vec<String> = serde_json::from_slice(&bytes)?;
        let mut sources = Vec::new();
        for mime in types {
            let step = format!("clipboard read {mime}");
            let result = helper_output(
                &["clipboard-read", &mime],
                deadline.min(Instant::now() + MIME_BUDGET),
                &step,
            );
            match result {
                Ok(bytes) => sources.push((mime, bytes)),
                Err(e) if strict => return Err(e),
                Err(_) => {} // Preserve the other MIME types if only this owner/type stalls.
            }
            if Instant::now() >= deadline {
                eprintln!("warning: clipboard save exceeded its 500 ms total budget");
                if strict {
                    return Err(timeout());
                }
                break;
            }
        }
        ensure!(
            !sources.is_empty() || bytes == b"[]",
            "clipboard save failed; no readable MIME types"
        );
        Ok(Self { sources })
    }
    pub fn restore(&self) -> Result<()> {
        self.restore_until(Instant::now() + STEP_BUDGET)
    }
    fn restore_until(&self, deadline: Instant) -> Result<()> {
        let deadline = deadline.min(Instant::now() + STEP_BUDGET);
        if self.sources.is_empty() {
            helper_output(&["clipboard-clear"], deadline, "clipboard clear")?;
            return Ok(());
        }
        self.serve_until(deadline, false).map(|_| ())
    }
    fn serve_until(&self, deadline: Instant, hang: bool) -> Result<u32> {
        warn_timeout(
            "clipboard-serve READY handshake",
            (|| {
                check(deadline)?;
                let mut command = Command::new(std::env::current_exe()?);
                command.arg(if hang {
                    "clipboard-hang-serve"
                } else {
                    "clipboard-serve"
                });
                let mut child = Helper::spawn(&mut command)?;
                let json = serde_json::to_vec(&self.sources)?;
                write_pipe(
                    &mut child.child().stdin.take().context("worker stdin")?,
                    &json,
                    deadline,
                )?;
                // READY is a line, not EOF: the owner keeps running after the handshake.
                let mut stdout = child.child().stdout.take().context("worker stdout")?;
                nonblocking(&stdout)?;
                let mut ready = [0; 6];
                let mut n = 0;
                while n < ready.len() {
                    wait(&stdout, PollFlags::IN, deadline)?;
                    match stdout.read(&mut ready[n..]) {
                        Ok(0) => anyhow::bail!("clipboard worker exited before READY"),
                        Ok(count) => n += count,
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(e) => return Err(e.into()),
                    }
                }
                ensure!(
                    &ready == b"READY\n",
                    "clipboard worker failed READY handshake"
                );
                Ok(child.keep())
            })(),
        )
    }
    pub fn same(&self, other: &Self) -> bool {
        self.sources.len() == other.sources.len()
            && self.sources.iter().all(|s| other.sources.contains(s))
    }
}
fn copy_text(text: &str, deadline: Instant) -> Result<()> {
    let deadline = deadline.min(Instant::now() + STEP_BUDGET);
    warn_timeout(
        "wl-copy write/ownership",
        (|| {
            check(deadline)?;
            let mut child = Helper::spawn(
                Command::new("wl-copy")
                    .args(["--type", "text/plain;charset=utf-8"])
                    .stderr(Stdio::null()),
            )?;
            write_pipe(
                &mut child.child().stdin.take().context("wl-copy stdin")?,
                text.as_bytes(),
                deadline,
            )?;
            child.finish(deadline)
        })(),
    )
}
#[derive(Clone)]
pub struct Deadline {
    end: Instant,
    cancelled: Arc<AtomicBool>,
}
impl Deadline {
    pub fn new(budget: Duration) -> Self {
        Self {
            end: Instant::now() + budget,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }
    fn ending_at(&self, end: Instant) -> Self {
        Self {
            end,
            cancelled: self.cancelled.clone(),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            return Err(timeout());
        }
        check(self.end)
    }
    fn sleep(&self, delay: Duration, step: &str) -> Result<()> {
        warn_timeout(
            step,
            (|| {
                self.check()?;
                thread::sleep(delay.min(self.end.saturating_duration_since(Instant::now())));
                self.check()
            })(),
        )
    }
}
pub fn insert(
    text: &str,
    keyboard: &mut Keyboard,
    restore_delay_ms: u64,
    deadline: Deadline,
) -> Result<()> {
    insert_with(text, |end| keyboard.paste(end), restore_delay_ms, deadline)
}
fn insert_with(
    text: &str,
    paste: impl FnOnce(&Deadline) -> Result<()>,
    restore_delay_ms: u64,
    deadline: Deadline,
) -> Result<()> {
    insert_phases(
        |end| Clipboard::save_until(end, false),
        |end| copy_text(text, end),
        paste,
        |original, end| original.restore_until(end),
        restore_delay_ms,
        deadline,
    )
}
// Save/copy/paste may exhaust their phase, but cannot spend the cleanup reserve.
// The callbacks also let tests exercise the real orchestration without a desktop.
fn insert_phases<T>(
    save: impl FnOnce(Instant) -> Result<T>,
    copy: impl FnOnce(Instant) -> Result<()>,
    paste: impl FnOnce(&Deadline) -> Result<()>,
    restore: impl FnOnce(&T, Instant) -> Result<()>,
    restore_delay_ms: u64,
    deadline: Deadline,
) -> Result<()> {
    ensure!(
        restore_delay_ms <= crate::config::MAX_RESTORE_DELAY_MS,
        "restore_delay_ms must be 0–1000"
    );
    deadline.check()?;
    let delay = Duration::from_millis(restore_delay_ms);
    let reserve = delay + RESTORE_BUDGET + RESTORE_MARGIN;
    let paste_deadline = deadline.ending_at(
        deadline
            .end
            .checked_sub(reserve)
            .context("insertion budget has no clipboard restore reserve")?,
    );
    paste_deadline.check()?;
    let original = match save(paste_deadline.end) {
        Ok(original) => Some(original),
        Err(_) => {
            eprintln!("warning: clipboard save failed; pasting without clipboard restoration");
            None
        }
    };
    let mut changed = false;
    let result = (|| {
        paste_deadline.check()?;
        changed = true; // wl-copy can claim ownership before it reports failure.
        copy(paste_deadline.end)?;
        paste_deadline.sleep(Duration::from_millis(60), "delay before Ctrl+V")?;
        paste_deadline.check()?; // Both cancellation and the earlier deadline prohibit late Ctrl+V.
        paste(&paste_deadline)?;
        deadline.sleep(delay, "clipboard restore delay")
    })();
    let restored = if changed {
        if let Some(original) = original {
            restore(&original, deadline.end)
        } else {
            Ok(())
        }
    } else {
        Ok(())
    };
    result?;
    restored
}

pub fn stall_reader() -> Result<()> {
    let (_pipe, _) = paste::get_contents(
        paste::ClipboardType::Regular,
        paste::Seat::Unspecified,
        paste::MimeType::Specific("application/x-hark-large"),
    )?;
    println!("READY");
    std::io::stdout().flush()?;
    // Retain the read end without draining it until the parent closes stdin.
    std::io::stdin().read_to_end(&mut Vec::new())?;
    Ok(())
}
fn blocked_reader() -> Result<Helper> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("clipboard-stall-reader");
    let mut child = Helper::spawn(&mut command)?;
    let deadline = Instant::now() + STEP_BUDGET;
    let mut stdout = child
        .child()
        .stdout
        .take()
        .context("stalled reader stdout")?;
    nonblocking(&stdout)?;
    let mut ready = [0; 6];
    let mut n = 0;
    while n < ready.len() {
        wait(&stdout, PollFlags::IN, deadline)?;
        match stdout.read(&mut ready[n..]) {
            Ok(0) => anyhow::bail!("stalled reader exited before READY"),
            Ok(count) => n += count,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(e) => return Err(e.into()),
        }
    }
    ensure!(&ready == b"READY\n", "stalled reader not ready");
    Ok(child)
}
fn owner_exited(pid: u32) -> Result<()> {
    let deadline = Instant::now() + STEP_BUDGET;
    while std::path::Path::new(&format!("/proc/{pid}")).exists() {
        check(deadline).context("clipboard owner did not exit after ownership loss")?;
        thread::sleep(Duration::from_millis(2));
    }
    Ok(())
}
pub fn clipboard_roundtrip() -> Result<()> {
    // Strict backup for tests: do not replace an original clipboard we cannot restore.
    let original = Clipboard::save()?;
    let synthetic = Clipboard {
        sources: vec![
            (
                "text/plain;charset=utf-8".into(),
                b"hark synthetic clipboard value".to_vec(),
            ),
            ("application/x-hark-test".into(), vec![0, 1, 255, 42]),
            ("application/x-hark-large".into(), vec![42; 256 * 1024]),
        ],
    };
    let result = (|| {
        let owner = synthetic.serve_until(Instant::now() + STEP_BUDGET, false)?;
        ensure!(
            synthetic.same(&Clipboard::save()?),
            "synthetic MIME setup differs"
        );
        let stalled = blocked_reader()?;
        let a = thread::spawn(Clipboard::save);
        let b = thread::spawn(Clipboard::save);
        ensure!(
            synthetic.same(&a.join().unwrap()?) && synthetic.same(&b.join().unwrap()?),
            "concurrent clipboard requests differ"
        );
        drop(stalled);
        println!("clipboard concurrent/repeated reads with blocked recipient: PASS");
        copy_text("hark temporary replacement", Instant::now() + STEP_BUDGET)?;
        owner_exited(owner)?;
        println!("clipboard owner exits on ownership loss: PASS");
        synthetic.restore()?;
        ensure!(
            synthetic.same(&Clipboard::save()?),
            "synthetic MIME round trip differs"
        );
        Ok(())
    })();
    original.restore()?;
    result?;
    ensure!(
        original.same(&Clipboard::save()?),
        "original clipboard restoration differs"
    );
    println!("clipboard MIME save/restore: PASS; original restored");
    Ok(())
}

pub fn read_clipboard(mime: Option<&str>) -> Result<()> {
    if let Some(mime) = mime {
        let (mut pipe, _) = paste::get_contents(
            paste::ClipboardType::Regular,
            paste::Seat::Unspecified,
            paste::MimeType::Specific(mime),
        )?;
        let mut bytes = Vec::new();
        read_pipe(&mut pipe, &mut bytes, Instant::now() + MIME_BUDGET)?;
        std::io::stdout().write_all(&bytes)?;
    } else {
        let mut types: Vec<String> =
            match paste::get_mime_types(paste::ClipboardType::Regular, paste::Seat::Unspecified) {
                Ok(types) => types.into_iter().collect(),
                Err(paste::Error::ClipboardEmpty | paste::Error::NoSeats) => Vec::new(),
                Err(e) => return Err(e.into()),
            };
        types.sort();
        serde_json::to_writer(std::io::stdout(), &types)?;
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn simulated_insertion(
        save_delay: Duration,
        copy_delay: Duration,
        restore_delay: u64,
        budget: Duration,
        cancel: bool,
        expire: bool,
    ) -> (Result<()>, bool, bool, String) {
        use std::cell::{Cell, RefCell};
        let clipboard = RefCell::new("original clipboard".to_owned());
        let pasted = Cell::new(false);
        let restored = Cell::new(false);
        let deadline = Deadline::new(budget);
        let result = insert_phases(
            |end| {
                thread::sleep(save_delay);
                check(end)?;
                Ok(clipboard.borrow().clone())
            },
            |end| {
                *clipboard.borrow_mut() = "dictation".into();
                let delay = if expire {
                    end.saturating_duration_since(Instant::now()) + Duration::from_millis(5)
                } else {
                    copy_delay
                };
                thread::sleep(delay);
                if cancel {
                    deadline.cancel();
                }
                check(end)
            },
            |end| {
                end.check()?;
                pasted.set(true);
                Ok(())
            },
            |original, end| {
                // This checks that save/copy/delay did not consume the restore allowance.
                assert!(end.saturating_duration_since(Instant::now()) >= RESTORE_BUDGET);
                thread::sleep(Duration::from_millis(100));
                check(end)?;
                *clipboard.borrow_mut() = original.clone();
                restored.set(true);
                Ok(())
            },
            restore_delay,
            deadline.clone(),
        );
        (result, pasted.get(), restored.get(), clipboard.into_inner())
    }
    #[test]
    fn slow_save_and_copy_preserve_restore_budget() {
        let (result, pasted, restored, clipboard) = simulated_insertion(
            Duration::from_millis(480),
            Duration::from_millis(480),
            60,
            INSERT_BUDGET,
            false,
            false,
        );
        result.unwrap();
        assert!(pasted && restored);
        assert_eq!(clipboard, "original clipboard");
    }
    #[test]
    fn maximum_restore_delay_still_restores() {
        let started = Instant::now();
        let (result, pasted, restored, clipboard) = simulated_insertion(
            Duration::from_millis(10),
            Duration::from_millis(10),
            1000,
            INSERT_BUDGET,
            false,
            false,
        );
        result.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(1000));
        assert!(pasted && restored);
        assert_eq!(clipboard, "original clipboard");
    }
    #[test]
    fn exhausted_paste_phase_still_restores_without_pasting() {
        let (result, pasted, restored, clipboard) = simulated_insertion(
            Duration::from_millis(100),
            Duration::ZERO,
            60,
            Duration::from_millis(900),
            false,
            true,
        );
        assert!(result.is_err());
        assert!(!pasted && restored);
        assert_eq!(clipboard, "original clipboard");
    }
    #[test]
    fn cancelled_after_copy_restores_and_never_pastes() {
        let (result, pasted, restored, clipboard) = simulated_insertion(
            Duration::ZERO,
            Duration::ZERO,
            60,
            INSERT_BUDGET,
            true,
            false,
        );
        assert!(result.is_err());
        assert!(!pasted && restored);
        assert_eq!(clipboard, "original clipboard");
    }
    #[tokio::test]
    async fn overall_deadline_returns_and_cancels_late_paste() {
        let pasted = Arc::new(AtomicBool::new(false));
        let worker_pasted = pasted.clone();
        let started = Instant::now();
        let result = bounded_with_budget(Duration::from_millis(50), move |deadline| {
            thread::sleep(Duration::from_millis(120));
            deadline.check()?;
            worker_pasted.store(true, Ordering::Relaxed);
            Ok(())
        })
        .await;
        assert!(result.is_err());
        assert!(started.elapsed() < Duration::from_millis(110));
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(!pasted.load(Ordering::Relaxed));
    }
    #[test]
    fn stalled_writes_and_missing_ready_have_deadlines() {
        let (mut writer, _reader) = std::os::unix::net::UnixStream::pair().unwrap();
        let started = Instant::now();
        assert!(
            write_pipe(
                &mut writer,
                &vec![0; 1024 * 1024],
                started + Duration::from_millis(50)
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_millis(150));
        let (mut reader, _worker) = std::os::unix::net::UnixStream::pair().unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(wait(&reader, PollFlags::IN, deadline).is_err());
        // The production READY path uses the same wait before every read.
        assert!(read_pipe(&mut reader, &mut vec![], deadline).is_err());
    }
    #[test]
    fn never_closing_clipboard_pipe_is_bounded() {
        let (mut reader, mut owner) = std::os::unix::net::UnixStream::pair().unwrap();
        owner.write_all(b"partial clipboard").unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let worker = thread::spawn(move || {
            let started = std::time::Instant::now();
            let result = read_pipe(
                &mut reader,
                &mut vec![],
                started + Duration::from_millis(250),
            );
            tx.send((result.is_err(), started.elapsed())).unwrap();
        });
        let bounded = rx.recv_timeout(Duration::from_millis(750));
        drop(owner);
        worker.join().unwrap();
        let (timed_out, elapsed) = bounded.expect("clipboard blocked beyond its budget");
        assert!(timed_out);
        assert!(elapsed < Duration::from_millis(500));
    }
}

// The clipboard owner's Wayland dispatch must never wait for a recipient to
// drain a pipe. Each request owns its fd and has an independent bounded writer.
mod owner {
    use super::*;
    use std::{collections::HashMap, fs::File, sync::atomic::AtomicUsize};
    use wayland_client::{
        Connection, Dispatch, QueueHandle, delegate_noop,
        globals::{GlobalListContents, registry_queue_init},
        protocol::{wl_registry::WlRegistry, wl_seat::WlSeat},
    };
    use wayland_protocols::ext::data_control::v1::client::{
        ext_data_control_device_v1::{Event as DeviceEvent, ExtDataControlDeviceV1 as Device},
        ext_data_control_manager_v1::ExtDataControlManagerV1 as Manager,
        ext_data_control_offer_v1::ExtDataControlOfferV1 as Offer,
        ext_data_control_source_v1::{Event as SourceEvent, ExtDataControlSourceV1 as Source},
    };
    struct Owner {
        data: HashMap<String, Arc<Vec<u8>>>,
        cancelled: bool,
        writers: Arc<AtomicUsize>,
        hang: bool,
        held: Vec<File>,
    }
    impl Dispatch<WlRegistry, GlobalListContents> for Owner {
        fn event(
            _: &mut Self,
            _: &WlRegistry,
            _: wayland_client::protocol::wl_registry::Event,
            _: &GlobalListContents,
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
        }
    }
    delegate_noop!(Owner: ignore WlSeat);
    delegate_noop!(Owner: ignore Manager);
    delegate_noop!(Owner: ignore Offer);
    impl Dispatch<Device, ()> for Owner {
        wayland_client::event_created_child!(Owner, Device, [0 => (Offer, ())]);
        fn event(
            state: &mut Self,
            _: &Device,
            event: DeviceEvent,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                DeviceEvent::DataOffer { id } => id.destroy(),
                DeviceEvent::Finished => state.cancelled = true,
                _ => {}
            }
        }
    }
    impl Dispatch<Source, ()> for Owner {
        fn event(
            state: &mut Self,
            _: &Source,
            event: SourceEvent,
            _: &(),
            _: &Connection,
            _: &QueueHandle<Self>,
        ) {
            match event {
                SourceEvent::Cancelled => state.cancelled = true,
                SourceEvent::Send { mime_type, fd } => {
                    let mut pipe = File::from(fd);
                    let Some(bytes) = state.data.get(&mime_type).cloned() else {
                        return;
                    };
                    if state.hang {
                        state.held.push(pipe);
                        return;
                    }
                    // Limit concurrent writers even if a hostile consumer requests repeatedly.
                    if state.writers.fetch_add(1, Ordering::Relaxed) >= 16 {
                        state.writers.fetch_sub(1, Ordering::Relaxed);
                        return; // Dropping the fd sends EOF, including unknown/excess requests.
                    }
                    let writers = state.writers.clone();
                    thread::spawn(move || {
                        let _ = write_pipe(&mut pipe, &bytes, Instant::now() + INSERT_BUDGET);
                        drop(pipe);
                        writers.fetch_sub(1, Ordering::Relaxed);
                    });
                }
                _ => {}
            }
        }
    }
    pub fn serve(hang: bool) -> Result<()> {
        let data: Vec<(String, Vec<u8>)> = serde_json::from_reader(std::io::stdin())?;
        let conn = Connection::connect_to_env()?;
        let (globals, mut queue) = registry_queue_init::<Owner>(&conn)?;
        let qh = queue.handle();
        let manager: Manager = globals
            .bind(&qh, 1..=1, ())
            .context("clipboard ext data-control manager")?;
        let seat: WlSeat = globals.bind(&qh, 1..=7, ()).context("clipboard seat")?;
        let device = manager.get_data_device(&seat, &qh, ());
        let source = manager.create_data_source(&qh, ());
        let mut state = Owner {
            data: data.into_iter().map(|(m, b)| (m, Arc::new(b))).collect(),
            cancelled: false,
            writers: Arc::new(AtomicUsize::new(0)),
            hang,
            held: Vec::new(),
        };
        for mime in state.data.keys() {
            source.offer(mime.clone());
        }
        device.set_selection(Some(&source));
        queue.roundtrip(&mut state)?;
        ensure!(!state.cancelled, "clipboard ownership was rejected");
        println!("READY");
        std::io::stdout().flush()?;
        // After READY this owner can outlive the daemon/diagnostic. Do not keep
        // the caller's SSH/systemd --pipe descriptors open for its lifetime.
        let null = File::options().write(true).open("/dev/null")?;
        rustix::stdio::dup2_stdout(&null)?;
        rustix::stdio::dup2_stderr(&null)?;
        while !state.cancelled {
            queue.blocking_dispatch(&mut state)?;
        }
        // Worker process exits here: its remaining writers/held fds are all closed.
        Ok(())
    }
}
pub fn serve_clipboard(hang: bool) -> Result<()> {
    owner::serve(hang)
}
pub fn clear_clipboard() -> Result<()> {
    wl_clipboard_rs::copy::clear(
        wl_clipboard_rs::copy::ClipboardType::Regular,
        wl_clipboard_rs::copy::Seat::All,
    )?;
    Ok(())
}

pub async fn bounded_insertion<F>(work: F) -> Result<()>
where
    F: FnOnce(Deadline) -> Result<()> + Send + 'static,
{
    bounded_with_budget(INSERT_BUDGET, work).await
}
async fn bounded_with_budget<F>(budget: Duration, work: F) -> Result<()>
where
    F: FnOnce(Deadline) -> Result<()> + Send + 'static,
{
    let deadline = Deadline::new(budget);
    let worker_deadline = deadline.clone();
    let worker = tokio::task::spawn_blocking(move || work(worker_deadline));
    match tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.end), worker).await {
        Ok(result) => result.context("insertion worker failed")?,
        Err(_) => {
            deadline.cancel();
            eprintln!(
                "warning: insertion overall exceeded its {} ms time budget",
                budget.as_millis()
            );
            Err(timeout())
        }
    }
}

pub fn insert_print(text: String, restore_delay_ms: u64, deadline: Deadline) -> Result<()> {
    let printed = text.clone();
    insert_with(
        &text,
        move |end| {
            end.check()?;
            println!("{printed}");
            Ok(())
        },
        restore_delay_ms,
        deadline,
    )
}
pub async fn clipboard_hang_run(args: &[String]) -> Result<()> {
    ensure!(!args.is_empty(), "clipboard-hang-run needs a command");
    let original = Clipboard::save()?;
    let fake = Clipboard {
        sources: vec![(
            "text/plain;charset=utf-8".into(),
            b"synthetic stalled owner".to_vec(),
        )],
    };
    let result = (|| {
        fake.serve_until(Instant::now() + STEP_BUDGET, true)?;
        println!("synthetic never-closing clipboard owner: READY");
        let status = Command::new(&args[0]).args(&args[1..]).status()?;
        ensure!(status.success(), "stalled-owner test command failed");
        Ok(())
    })();
    original.restore()?;
    result?;
    ensure!(
        original.same(&Clipboard::save()?),
        "original clipboard restoration differs"
    );
    println!("never-closing owner run: PASS; original clipboard restored");
    Ok(())
}
pub async fn clipboard_hang_test() -> Result<()> {
    let original = Clipboard::save()?;
    let fake = Clipboard {
        sources: vec![(
            "text/plain;charset=utf-8".into(),
            b"synthetic stalled owner".to_vec(),
        )],
    };
    let result = async {
        fake.serve_until(Instant::now() + STEP_BUDGET, true)?;
        let started = Instant::now();
        bounded_insertion(|deadline| {
            insert_with(
                "synthetic safe replacement",
                |end| {
                    end.check()?;
                    println!("stalled-owner insertion: print only; no keystrokes");
                    Ok(())
                },
                60,
                deadline,
            )
        })
        .await?;
        ensure!(
            started.elapsed() < INSERT_BUDGET,
            "stalled owner exceeded insertion deadline"
        );
        println!(
            "never-closing clipboard owner: PASS; insertion finished in {} ms",
            started.elapsed().as_millis()
        );
        Ok(())
    }
    .await;
    original.restore()?;
    result?;
    ensure!(
        original.same(&Clipboard::save()?),
        "original clipboard restoration differs"
    );
    println!("never-closing owner: original clipboard restored");
    Ok(())
}
