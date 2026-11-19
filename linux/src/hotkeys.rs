use crate::config::Shortcut;
use anyhow::{Result, bail, ensure};
use futures_util::StreamExt;
use tokio::sync::mpsc;
pub const APP: &str = "cz.hajadlo.hark";
pub const TEST_APP: &str = "cz.hajadlo.hark.Test";
const KDE: &str = "org.kde.kglobalaccel";
#[derive(Debug)]
pub struct Event {
    pub key: String,
    pub pressed: bool,
    pub received: std::time::Instant,
}
pub fn qt_key(chord: &str) -> Result<i32> {
    let mut modifiers = 0;
    let mut key = None;
    for part in chord.split('+') {
        match part.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= 0x04000000,
            "shift" => modifiers |= 0x02000000,
            "alt" => modifiers |= 0x08000000,
            "meta" | "super" => modifiers |= 0x10000000,
            "space" => {
                ensure!(key.is_none(), "multiple keys in shortcut");
                key = Some(0x20)
            }
            "escape" => {
                ensure!(key.is_none(), "multiple keys in shortcut");
                key = Some(0x01000000)
            }
            p => {
                ensure!(key.is_none(), "multiple keys in shortcut");
                key = Some(
                    if let Some(f) = p
                        .strip_prefix('f')
                        .and_then(|n| n.parse::<i32>().ok())
                        .filter(|n| (1..=35).contains(n))
                    {
                        0x01000030 + f - 1
                    } else if p.len() == 1 && p.as_bytes()[0].is_ascii_alphanumeric() {
                        p.as_bytes()[0].to_ascii_uppercase() as i32
                    } else {
                        bail!(
                            "unsupported shortcut key (use Space, Escape, F1–F35, letter or digit)"
                        )
                    },
                );
            }
        }
    }
    ensure!(modifiers != 0, "shortcut needs a modifier");
    Ok(modifiers | key.ok_or_else(|| anyhow::anyhow!("shortcut needs a key"))?)
}
async fn proxy(conn: &zbus::Connection) -> Result<zbus::Proxy<'_>> {
    Ok(zbus::Proxy::new(conn, KDE, "/kglobalaccel", "org.kde.KGlobalAccel").await?)
}
fn action(app: &str, index: usize) -> Vec<String> {
    vec![
        app.into(),
        format!("dictation-{index}"),
        "Hark".into(),
        format!("Dictation {index}"),
    ]
}
fn handy_present() -> bool {
    std::fs::read_dir("/proc").is_ok_and(|entries| {
        entries.flatten().any(|entry| {
            std::fs::read_to_string(entry.path().join("comm"))
                .is_ok_and(|name| name.trim().eq_ignore_ascii_case("handy"))
        })
    })
}
async fn wait_for_handy<P, D, F>(mut present: P, mut delay: D)
where
    P: FnMut() -> bool,
    D: FnMut() -> F,
    F: std::future::Future<Output = ()>,
{
    let mut logged = false;
    while present() {
        if !logged {
            eprintln!("Handy is running; waiting to bind shortcuts (checking every 5 s)");
            logged = true;
        }
        delay().await;
    }
    if logged {
        eprintln!("Handy has exited; binding hark shortcuts");
    }
}
pub async fn check(shortcuts: &[Shortcut]) -> Result<()> {
    if shortcuts.len() == 1 && shortcuts[0].key == "Ctrl+Alt+Shift+F10" {
        match check_for(shortcuts, TEST_APP).await {
            Ok(()) => Ok(()),
            Err(_) => check_for(shortcuts, APP).await,
        }
    } else {
        check_for(shortcuts, APP).await
    }
}
async fn check_for(shortcuts: &[Shortcut], app: &str) -> Result<()> {
    // Handy's shortcuts are invisible in KDE's action lookup on this desktop.
    // Guard its two known chords explicitly rather than stealing them silently.
    let uses_handy_chord = shortcuts
        .iter()
        .any(|s| matches!(qt_key(&s.key), Ok(0x04000020 | 0x06000020)));
    let handy_running = handy_present();
    ensure!(
        !uses_handy_chord || !handy_running,
        "quit Handy before binding Ctrl+Space or Ctrl+Shift+Space"
    );
    let conn = zbus::Connection::session().await?;
    let p = proxy(&conn).await?;
    for s in shortcuts {
        let key = qt_key(&s.key)?;
        let owner: Vec<String> = p.call("action", &key).await?;
        ensure!(
            owner.is_empty() || owner.first().map(String::as_str) == Some(app),
            "shortcut {} is occupied; quit Handy before starting hark",
            s.key
        );
    }
    Ok(())
}
pub struct Backend {
    conn: zbus::Connection,
    app: &'static str,
    owner: String,
    actions: Vec<Vec<String>>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}
async fn monitor_owner(
    conn: &zbus::Connection,
    tx: mpsc::Sender<Result<Event>>,
) -> Result<(String, tokio::task::JoinHandle<()>)> {
    let bus = zbus::Proxy::new(
        conn,
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
    )
    .await?;
    // Subscribe before looking up the owner or binding, so a setup-time restart
    // cannot leave a daemon that appears healthy with no registered shortcuts.
    let mut owners = bus
        .receive_signal_with_args("NameOwnerChanged", &[(0, KDE)])
        .await?;
    let owner: String = bus.call("GetNameOwner", &KDE).await?;
    let task = tokio::spawn(async move {
        let error = match owners.next().await {
            Some(signal) => match signal.body().deserialize::<(String, String, String)>() {
                Ok((_, old, new)) => anyhow::anyhow!(
                    "{KDE} owner changed ({old} → {}); exiting so systemd re-registers shortcuts",
                    if new.is_empty() { "absent" } else { &new }
                ),
                Err(e) => anyhow::anyhow!("{KDE} owner watch failed: {e}"),
            },
            None => {
                anyhow::anyhow!("{KDE} owner watch disconnected; exiting to re-register shortcuts")
            }
        };
        let _ = tx.send(Err(error)).await;
    });
    Ok((owner, task))
}
impl Backend {
    pub async fn bind(shortcuts: &[Shortcut], tx: mpsc::Sender<Result<Event>>) -> Result<Self> {
        Self::bind_for(shortcuts, tx, APP).await
    }
    pub async fn bind_test(
        shortcuts: &[Shortcut],
        tx: mpsc::Sender<Result<Event>>,
    ) -> Result<Self> {
        ensure!(
            shortcuts.len() == 1 && shortcuts[0].key == "Ctrl+Alt+Shift+F10",
            "test daemon must use only the fixed test chord"
        );
        Self::bind_for(shortcuts, tx, TEST_APP).await
    }
    async fn bind_for(
        shortcuts: &[Shortcut],
        tx: mpsc::Sender<Result<Event>>,
        app: &'static str,
    ) -> Result<Self> {
        if app != TEST_APP {
            tokio::select! {
                _=wait_for_handy(handy_present, || tokio::time::sleep(std::time::Duration::from_secs(5)))=>{},
                _=shutdown()=>std::process::exit(0),
            }
        }
        check_for(shortcuts, app).await?;
        let conn = zbus::Connection::session().await?;
        // A second daemon must not take over our existing component's signals.
        conn.request_name(app).await?;
        let (owner, watcher) = monitor_owner(&conn, tx.clone()).await?;
        let mut backend = Self {
            conn,
            app,
            owner,
            actions: vec![],
            tasks: vec![watcher],
        };
        let result = backend.register(shortcuts, tx).await;
        if let Err(e) = result {
            backend.close().await;
            return Err(e);
        }
        Ok(backend)
    }
    async fn register(
        &mut self,
        shortcuts: &[Shortcut],
        tx: mpsc::Sender<Result<Event>>,
    ) -> Result<()> {
        // Pin calls and cleanup to this unique owner; never unregister a new
        // KDE instance's bindings or activate a service that is shutting down.
        let p = zbus::Proxy::new(
            &self.conn,
            self.owner.as_str(),
            "/kglobalaccel",
            "org.kde.KGlobalAccel",
        )
        .await?;
        for (i, s) in shortcuts.iter().enumerate() {
            let a = action(self.app, i);
            let key = qt_key(&s.key)?;
            p.call::<_, _, ()>("doRegister", &a).await?;
            self.actions.push(a.clone());
            // SetPresent | NoAutoloading: honour config, not stale KDE persisted keys.
            let bound: Vec<i32> = p.call("setShortcut", &(&a, vec![key], 6u32)).await?;
            ensure!(
                bound == vec![key],
                "KDE did not bind {} (quit Handy first)",
                s.key
            );
        }
        let path: zbus::zvariant::OwnedObjectPath = p.call("getComponent", &self.app).await?;
        let component = zbus::Proxy::new(
            &self.conn,
            self.owner.clone(),
            path,
            "org.kde.kglobalaccel.Component",
        )
        .await?;
        let mut stream = component.receive_all_signals().await?;
        let bindings = shortcuts.to_vec();
        let app = self.app;
        let task = tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                let member = msg
                    .header()
                    .member()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                if !matches!(
                    member.as_str(),
                    "globalShortcutPressed" | "globalShortcutReleased"
                ) {
                    continue;
                }
                if let Ok((component, id, _)) = msg.body().deserialize::<(String, String, i64)>() {
                    if component != app {
                        continue;
                    }
                    if let Some(i) = id
                        .strip_prefix("dictation-")
                        .and_then(|v| v.parse::<usize>().ok())
                        && let Some(s) = bindings.get(i)
                        && tx
                            .send(Ok(Event {
                                key: s.key.clone(),
                                pressed: member == "globalShortcutPressed",
                                received: std::time::Instant::now(),
                            }))
                            .await
                            .is_err()
                    {
                        break;
                    }
                }
            }
        });
        self.tasks.push(task);
        for s in shortcuts {
            eprintln!("BOUND {} → {}", s.key, s.mode);
        }
        Ok(())
    }
    pub async fn close(&self) {
        for task in &self.tasks {
            task.abort();
        }
        if let Ok(p) = zbus::Proxy::new(
            &self.conn,
            self.owner.as_str(),
            "/kglobalaccel",
            "org.kde.KGlobalAccel",
        )
        .await
        {
            for a in &self.actions {
                let _ = p.call::<_, _, ()>("unRegister", a).await;
            }
        }
    }
}
pub async fn kde_spike() -> Result<()> {
    let (tx, mut rx) = mpsc::channel(64);
    let shortcuts = [Shortcut {
        key: "Ctrl+Alt+Shift+F10".into(),
        mode: "raw".into(),
    }];
    let backend = Backend::bind(&shortcuts, tx).await?;
    let clock = std::time::Instant::now();
    let result = async { loop {
        tokio::select! {
            event=rx.recv()=> {
                let e=event.ok_or_else(||anyhow::anyhow!("shortcut backend disconnected"))??;
                eprintln!("{} ms {} {}",e.received.saturating_duration_since(clock).as_millis(),if e.pressed {"Pressed"} else {"Released"},e.key);
            },
            _=shutdown()=>break,
        }
    } Ok(()) }.await;
    backend.close().await;
    result
}
pub async fn portal_spike() -> Result<()> {
    use ashpd::desktop::global_shortcuts::{GlobalShortcuts, NewShortcut};
    let conn = zbus::Connection::session().await?;
    let registry = zbus::Proxy::new(
        &conn,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.host.portal.Registry",
    )
    .await?;
    let options: std::collections::HashMap<&str, zbus::zvariant::Value<'_>> = Default::default();
    registry
        .call::<_, _, ()>("Register", &(APP, options))
        .await?;
    eprintln!("registered {APP}");
    let p = GlobalShortcuts::with_connection(conn).await?;
    let session = p.create_session(Default::default()).await?;
    let mut press = p.receive_activated().await?;
    let mut release = p.receive_deactivated().await?;
    let shortcuts =
        [NewShortcut::new("test", "Hark test dictation").preferred_trigger("CTRL+ALT+SHIFT+F10")];
    eprintln!("BindShortcuts: waiting for confirmation (Ctrl+C cancels)");
    let binding = tokio::select! {r=p.bind_shortcuts(&session,&shortcuts,None,Default::default())=>r?,_=shutdown()=>{session.close().await?;return Ok(());}};
    eprintln!("BOUND {:?}", binding.response()?);
    let clock = std::time::Instant::now();
    loop {
        tokio::select! {Some(e)=press.next()=>eprintln!("{} ms Activated {e:?}",clock.elapsed().as_millis()),Some(e)=release.next()=>eprintln!("{} ms Deactivated {e:?}",clock.elapsed().as_millis()),_=shutdown()=>break}
    }
    session.close().await?;
    Ok(())
}
pub async fn shutdown() {
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("SIGTERM handler");
    tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn handy_present_waits_then_allows_binding() {
        let probes = std::cell::Cell::new(0);
        let waits = std::cell::Cell::new(0);
        let bound = std::cell::Cell::new(false);
        wait_for_handy(
            || {
                assert!(!bound.get());
                let n = probes.get();
                probes.set(n + 1);
                n < 3
            },
            || {
                waits.set(waits.get() + 1);
                std::future::ready(())
            },
        )
        .await;
        bound.set(true);
        assert_eq!(waits.get(), 3);
        assert_eq!(probes.get(), 4);
        assert!(bound.get());
    }
    #[tokio::test]
    async fn owner_monitor_detects_loss_and_replacement_on_private_bus() {
        use std::{
            io::{BufRead, BufReader},
            process::{Command, Stdio},
        };
        struct Bus(std::process::Child);
        impl Drop for Bus {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        for replacement in [false, true] {
            // Use an isolated bus; never restart the user's live KDE service.
            let mut bus = Bus(Command::new("dbus-daemon")
                .args(["--session", "--nofork", "--print-address=1"])
                .stdout(Stdio::piped())
                .spawn()
                .expect("private test dbus-daemon"));
            let mut address = String::new();
            BufReader::new(bus.0.stdout.take().unwrap())
                .read_line(&mut address)
                .unwrap();
            let owner = zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build()
                .await
                .unwrap();
            let observer = zbus::connection::Builder::address(address.trim())
                .unwrap()
                .build()
                .await
                .unwrap();
            let proxy = zbus::fdo::DBusProxy::new(&owner).await.unwrap();
            proxy
                .inner()
                .call::<_, _, u32>("RequestName", &(KDE, 1u32))
                .await
                .unwrap();
            let (tx, mut rx) = mpsc::channel(1);
            let (_, watcher) = monitor_owner(&observer, tx).await.unwrap();
            let successor = if replacement {
                let successor = zbus::connection::Builder::address(address.trim())
                    .unwrap()
                    .build()
                    .await
                    .unwrap();
                zbus::fdo::DBusProxy::new(&successor)
                    .await
                    .unwrap()
                    .inner()
                    .call::<_, _, u32>("RequestName", &(KDE, 2u32))
                    .await
                    .unwrap();
                Some(successor)
            } else {
                let released: u32 = proxy.inner().call("ReleaseName", &KDE).await.unwrap();
                assert_eq!(released, 1);
                None
            };
            let error = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .to_string();
            assert!(error.contains("org.kde.kglobalaccel owner changed"));
            assert!(error.contains("exiting so systemd re-registers shortcuts"));
            assert_eq!(error.contains("absent"), !replacement);
            watcher.await.unwrap();
            drop(successor);
        }
    }
    #[test]
    fn qt_codes() {
        assert_eq!(qt_key("Ctrl+Alt+Shift+F10").unwrap(), 251658297);
        assert_eq!(qt_key("Ctrl+Space").unwrap(), 0x04000020);
        assert!(qt_key("Space").is_err());
        assert!(qt_key("Ctrl+bad").is_err());
    }
}
