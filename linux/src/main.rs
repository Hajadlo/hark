mod audio;
mod config;
mod daemon;
mod feedback;
mod hotkeys;
mod insert;
mod overlay;
mod state;
mod upload;
mod wav;
use anyhow::{Context, Result, bail, ensure};
use config::Config;
use std::{path::PathBuf, time::Duration};
#[tokio::main(worker_threads = 2)]
async fn main() {
    if let Err(e) = dispatch().await {
        eprintln!("hark: {e:#}");
        std::process::exit(1);
    }
}
async fn dispatch() -> Result<()> {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut path = config::default_path();
    if let Some(i) = args.iter().position(|a| a == "--config") {
        ensure!(i + 1 < args.len(), "--config needs a path");
        path = PathBuf::from(args.remove(i + 1));
        args.remove(i);
    }
    let command = args.first().map(String::as_str).unwrap_or("help");
    if command == "overlay-previews" {
        let directory = args
            .get(1)
            .context("overlay-previews needs an output directory")?;
        return overlay::previews(std::path::Path::new(directory));
    }
    if command == "notification-test" {
        return feedback::probe().await;
    }
    if command == "clipboard-stall-reader" {
        return insert::stall_reader();
    }
    if command == "clipboard-hang-run" {
        return insert::clipboard_hang_run(&args[1..]).await;
    }
    if command == "clipboard-hang-serve" {
        return insert::serve_clipboard(true);
    }
    if command == "clipboard-types" {
        return insert::read_clipboard(None);
    }
    if command == "clipboard-read" {
        return insert::read_clipboard(Some(
            args.get(1).context("clipboard-read needs MIME type")?,
        ));
    }
    if command == "clipboard-clear" {
        return insert::clear_clipboard();
    }
    if command == "clipboard-hang-test" {
        return insert::clipboard_hang_test().await;
    }
    if command == "clipboard-serve" {
        return insert::serve_clipboard(false);
    }
    if command == "hotkey-spike" {
        return hotkeys::kde_spike().await;
    }
    if command == "portal-spike" {
        return hotkeys::portal_spike().await;
    }
    if command == "clipboard-test" {
        return insert::clipboard_roundtrip();
    }
    if command == "audio-devices" {
        for name in audio::devices()? {
            println!("{name}");
        }
        return Ok(());
    }
    if command == "test-chord" {
        let chord = config::Shortcut {
            key: "Ctrl+Alt+Shift+F10".into(),
            mode: "raw".into(),
        };
        let conn = zbus::Connection::session().await?;
        let p = zbus::Proxy::new(
            &conn,
            "org.kde.kglobalaccel",
            "/kglobalaccel",
            "org.kde.KGlobalAccel",
        )
        .await?;
        let owner: Vec<String> = p.call("action", &hotkeys::qt_key(&chord.key)?).await?;
        ensure!(
            matches!(
                owner.first().map(String::as_str),
                Some(hotkeys::APP | hotkeys::TEST_APP)
            ),
            "refusing injection: test chord is not bound to hark"
        );
        let ms = args
            .get(1)
            .map(|s| s.parse::<u64>())
            .transpose()?
            .unwrap_or(1000);
        ensure!(ms <= 10_000, "test hold too long");
        let mut keyboard = insert::Keyboard::new()?;
        let owner: Vec<String> = p.call("action", &hotkeys::qt_key(&chord.key)?).await?;
        ensure!(
            matches!(
                owner.first().map(String::as_str),
                Some(hotkeys::APP | hotkeys::TEST_APP)
            ),
            "refusing injection: hark binding disappeared during keyboard discovery"
        );
        return keyboard.test_chord(Duration::from_millis(ms));
    }
    if !matches!(command, "run" | "transcribe" | "check") {
        println!(
            "hark run | transcribe <file.wav> [--mode MODE] | check\n  --config PATH\n  hotkey-spike | portal-spike | audio-devices | clipboard-test | test-chord [hold-ms]\n  overlay-previews <directory>"
        );
        return Ok(());
    }
    let config = Config::load(&path)?;
    match command {
        "run" => {
            let test_insertion = args.get(1).is_some_and(|s| s == "--test-insertion");
            ensure!(
                !test_insertion || (config.device == "test" && config.insert == "print"),
                "--test-insertion requires device=test and insert=print"
            );
            daemon::run(config, test_insertion).await
        }
        "transcribe" => {
            let file = args.get(1).context("transcribe needs a WAV path")?;
            let mode = args
                .iter()
                .position(|s| s == "--mode")
                .map(|i| args.get(i + 1).context("--mode needs a value"))
                .transpose()?
                .map(String::as_str)
                .unwrap_or("clean");
            ensure!(config::valid_name(mode), "invalid mode");
            ensure!(
                std::fs::metadata(file)?.len() <= 64 * 1024 * 1024,
                "file exceeds 64 MiB"
            );
            let bytes = std::fs::read(file)?;
            ensure!(bytes.len() <= 64 * 1024 * 1024, "file exceeds 64 MiB");
            println!(
                "{}",
                upload::Uploader::new(&config)?
                    .transcribe(&wav::from_wav(&bytes)?, mode)
                    .await?
            );
            Ok(())
        }
        "check" => check(&config).await,
        _ => unreachable!(),
    }
}
async fn check(c: &Config) -> Result<()> {
    println!("config: valid, mode 600; device={}", c.device);
    let mut failures = vec![];
    match upload::Uploader::new(c)?.models().await {
        Ok(j) => println!(
            "server models: {}",
            j["data"].as_array().map_or(0, Vec::len)
        ),
        Err(e) => failures.push(e.to_string()),
    }
    match hotkeys::check(&c.shortcuts).await {
        Ok(()) => println!("KDE shortcut backend: available"),
        Err(e) => failures.push(e.to_string()),
    }
    match insert::Keyboard::new() {
        Ok(_) => println!("uinput: available"),
        Err(e) => failures.push(e.to_string()),
    }
    for tool in ["wl-copy", "wl-paste", "pw-play"] {
        if std::process::Command::new("which")
            .arg(tool)
            .stdout(std::process::Stdio::null())
            .status()?
            .success()
        {
            println!("{tool}: available");
        } else {
            failures.push(format!("missing {tool}"));
        }
    }
    for message in &failures {
        eprintln!("check: {}", c.redact(message));
    }
    if !failures.is_empty() {
        bail!("{} checks failed", failures.len());
    }
    Ok(())
}
