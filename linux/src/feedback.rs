use anyhow::Result;
use futures_util::StreamExt;
use std::{
    collections::HashMap,
    sync::{
        Mutex,
        atomic::{AtomicU32, Ordering},
    },
};
use tokio::sync::mpsc;
#[derive(Debug)]
pub enum Action {
    Retry,
    Discard,
}
struct Display {
    conn: zbus::Connection,
    tx: mpsc::Sender<Action>,
    id: AtomicU32,
    watcher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}
impl Display {
    pub async fn new(tx: mpsc::Sender<Action>) -> Result<Self> {
        Ok(Self::with_connection(
            zbus::Connection::session().await?,
            tx,
        ))
    }
    fn with_connection(conn: zbus::Connection, tx: mpsc::Sender<Action>) -> Self {
        Self {
            conn,
            tx,
            id: AtomicU32::new(0),
            watcher: Mutex::new(None),
        }
    }
    pub async fn error(&self, message: &str, retry: bool) -> Result<()> {
        self.stop_watcher();
        let proxy = zbus::Proxy::new(
            &self.conn,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await?;
        let mut stream = proxy.receive_signal("ActionInvoked").await?;
        let actions: Vec<&str> = if retry {
            vec!["retry", "Retry", "discard", "Discard"]
        } else {
            vec![]
        };
        let mut hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
        if retry {
            // A busy Retry is ignored; keep its action available for a later click.
            hints.insert("resident", true.into());
        }
        let id: u32 = proxy
            .call(
                "Notify",
                &(
                    "Hark",
                    self.id.load(Ordering::Relaxed),
                    "audio-input-microphone",
                    "Dictation failed",
                    message,
                    actions,
                    hints,
                    0i32,
                ),
            )
            .await?;
        self.id.store(id, Ordering::Relaxed);
        if retry {
            let tx = self.tx.clone();
            let watcher = tokio::spawn(async move {
                while let Some(signal) = stream.next().await {
                    if let Ok((notification, action)) = signal.body().deserialize::<(u32, String)>()
                        && notification == id
                    {
                        let result = match action.as_str() {
                            "retry" => Action::Retry,
                            "discard" => Action::Discard,
                            _ => continue,
                        };
                        if tx.send(result).await.is_err() {
                            break;
                        }
                    }
                }
            });
            *self.watcher.lock().expect("notification watcher lock") = Some(watcher);
        }
        Ok(())
    }
    fn stop_watcher(&self) {
        if let Some(watcher) = self
            .watcher
            .lock()
            .expect("notification watcher lock")
            .take()
        {
            watcher.abort();
        }
    }
    pub async fn close(&self) -> Result<()> {
        self.stop_watcher();
        let id = self.id.swap(0, Ordering::Relaxed);
        if id == 0 {
            return Ok(());
        }
        let proxy = zbus::Proxy::new(
            &self.conn,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.freedesktop.Notifications",
        )
        .await?;
        proxy.call::<_, _, ()>("CloseNotification", &id).await?;
        Ok(())
    }
}

enum Command {
    Error(String, bool),
    Close,
    Snapshot(tokio::sync::oneshot::Sender<u32>),
}
pub struct Notifications {
    commands: mpsc::UnboundedSender<Command>,
}
impl Notifications {
    pub fn new(tx: mpsc::Sender<Action>) -> Self {
        Self::with_display(tx, None)
    }
    fn with_display(tx: mpsc::Sender<Action>, initial: Option<Display>) -> Self {
        let (commands, mut rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            let mut display = initial;
            let mut pending = None;
            let mut last: Option<tokio::time::Instant> = None;
            loop {
                let due = last
                    .map(|t| t + std::time::Duration::from_secs(5))
                    .unwrap_or_else(tokio::time::Instant::now);
                tokio::select! {
                    command=rx.recv()=>match command {
                        Some(Command::Error(message,retry))=>pending=Some((message,retry)),
                        Some(Command::Close)=> {
                            pending=None;
                            if let Some(d)=&display { notification_result(tokio::time::timeout(std::time::Duration::from_millis(500),d.close()).await.map_err(anyhow::Error::from).and_then(|r|r)); }
                        },
                        Some(Command::Snapshot(reply))=> { let _=reply.send(display.as_ref().map(|d|d.id.load(Ordering::Relaxed)).unwrap_or(0)); },
                        None=>break,
                    },
                    _=tokio::time::sleep_until(due), if pending.is_some()=> {
                        let (message,retry)=pending.take().unwrap();
                        last=Some(tokio::time::Instant::now()); // Failed attempts also count against the rate limit.
                        let result=tokio::time::timeout(std::time::Duration::from_secs(1), async {
                            if display.is_none() { display=Some(Display::new(tx.clone()).await?); }
                            display.as_ref().unwrap().error(&message,retry).await
                        }).await.map_err(anyhow::Error::from).and_then(|r|r);
                        notification_result(result);
                    },
                }
            }
        });
        Self { commands }
    }
    // Enqueue only: notification generation and D-Bus failures cannot stall dictation.
    pub fn error(&self, message: &str, retry: bool) {
        let _ = self.commands.send(Command::Error(message.into(), retry));
    }
    pub fn close(&self) {
        let _ = self.commands.send(Command::Close);
    }
    async fn snapshot(&self) -> Result<u32> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.commands.send(Command::Snapshot(tx))?;
        Ok(rx.await?)
    }
}
fn notification_result(result: Result<()>) {
    if let Err(e) = result {
        eprintln!("warning: notification failed: {e}");
    }
}
pub async fn probe() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(1);
    let notifications = Notifications::new(tx);
    let result: Result<()> = async {
        notifications.error("Synthetic Hark notification test", true);
        let id = tokio::time::timeout(std::time::Duration::from_secs(3), async {
            loop {
                let id = notifications.snapshot().await?;
                if id != 0 {
                    break Ok::<_, anyhow::Error>(id);
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await??;
        let conn = zbus::Connection::session().await?;
        let proxy = zbus::Proxy::new(
            &conn,
            "org.freedesktop.Notifications",
            "/org/freedesktop/Notifications",
            "org.kde.NotificationManager",
        )
        .await?;
        for _ in 0..2 {
            proxy
                .call::<_, _, ()>("InvokeAction", &(id, "retry"))
                .await?;
            let action = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await?;
            anyhow::ensure!(
                matches!(action, Some(Action::Retry)),
                "notification Retry action was not delivered"
            );
        }
        println!("KDE notification Retry action (including repeated click): PASS");
        Ok(())
    }
    .await;
    notifications.close();
    let _ = notifications.snapshot().await;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader},
        process::{Command as Process, Stdio},
        sync::Arc,
        time::{Duration, Instant},
    };
    type Calls = Arc<Mutex<Vec<(u32, String, bool, Instant)>>>;
    struct TestNotifications {
        calls: Calls,
        fail: bool,
    }
    #[zbus::interface(name = "org.freedesktop.Notifications")]
    impl TestNotifications {
        #[allow(clippy::too_many_arguments)] // freedesktop Notify has eight fixed wire parameters.
        async fn notify(
            &self,
            _app: &str,
            replaces: u32,
            _icon: &str,
            _summary: &str,
            body: &str,
            actions: Vec<String>,
            _hints: HashMap<String, zbus::zvariant::OwnedValue>,
            _timeout: i32,
        ) -> zbus::fdo::Result<u32> {
            self.calls.lock().unwrap().push((
                replaces,
                body.into(),
                !actions.is_empty(),
                Instant::now(),
            ));
            if self.fail {
                Err(zbus::fdo::Error::Failed(
                    "synthetic notification failure".into(),
                ))
            } else {
                Ok(42)
            }
        }
        async fn close_notification(&self, _id: u32) {}
    }
    struct Bus(std::process::Child);
    impl Drop for Bus {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    async fn setup(fail: bool) -> (Notifications, Calls, Bus, zbus::Connection) {
        let mut bus = Bus(Process::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap());
        let mut address = String::new();
        BufReader::new(bus.0.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let server = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .name("org.freedesktop.Notifications")
            .unwrap()
            .serve_at(
                "/org/freedesktop/Notifications",
                TestNotifications {
                    calls: calls.clone(),
                    fail,
                },
            )
            .unwrap()
            .build()
            .await
            .unwrap();
        let client = zbus::connection::Builder::address(address.trim())
            .unwrap()
            .build()
            .await
            .unwrap();
        let (tx, _rx) = mpsc::channel(1);
        let notifications =
            Notifications::with_display(tx.clone(), Some(Display::with_connection(client, tx)));
        (notifications, calls, bus, server)
    }
    async fn wait_calls(calls: &Calls, n: usize) {
        tokio::time::timeout(Duration::from_secs(7), async {
            while calls.lock().unwrap().len() < n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn notification_failure_is_nonfatal_and_bursts_are_coalesced() {
        let (notifications, calls, _bus, _server) = setup(true).await;
        notifications.error("synthetic", true);
        wait_calls(&calls, 1).await;
        // Snapshot remains responsive after a D-Bus error; the worker did not exit.
        assert_eq!(notifications.snapshot().await.unwrap(), 0);
        for _ in 0..10 {
            notifications.error("latest synthetic failure", false);
        }
        assert_eq!(notifications.snapshot().await.unwrap(), 0);
        tokio::time::sleep(Duration::from_millis(40)).await;
        assert_eq!(calls.lock().unwrap().len(), 1);
        let mut gestures = crate::state::Gestures::new(300);
        assert_eq!(
            gestures.press("a", "raw", 0),
            crate::state::Effect::Start("raw".into())
        );
        notifications.close();
        notifications.snapshot().await.unwrap();
    }
    #[tokio::test]
    async fn latest_error_replaces_previous_after_five_seconds_without_retry_actions() {
        let (notifications, calls, _bus, _server) = setup(false).await;
        notifications.error("first retryable error", true);
        wait_calls(&calls, 1).await;
        assert_eq!(notifications.snapshot().await.unwrap(), 42);
        notifications.error("intermediate error", true);
        notifications.error("latest terminal error", false);
        wait_calls(&calls, 2).await;
        {
            let calls = calls.lock().unwrap();
            assert_eq!(calls.len(), 2);
            assert_eq!(calls[0].0, 0);
            assert_eq!(calls[1].0, 42);
            assert_eq!(calls[1].1, "latest terminal error");
            assert!(!calls[1].2);
            assert!(calls[1].3.duration_since(calls[0].3) >= Duration::from_millis(4900));
        }
        notifications.close();
        notifications.snapshot().await.unwrap();
    }
}
