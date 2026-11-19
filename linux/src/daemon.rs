use crate::{
    audio,
    config::Config,
    feedback, hotkeys, insert, overlay,
    state::{Effect, Gestures},
    upload::Uploader,
};
use anyhow::{Context, Result};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::mpsc;

#[derive(Clone)]
struct Pending {
    wav: Vec<u8>,
    mode: String,
}
#[derive(Default)]
struct Uploads {
    pending: Option<Pending>,
    in_flight: bool,
}
impl Uploads {
    fn discard(&mut self) {
        self.pending = None;
    }
    fn new_recording(&mut self) {
        self.discard();
    }
    fn begin(&mut self, pending: Pending) -> bool {
        // All daemon WAVs are our canonical 44-byte header, 16 kHz mono PCM16.
        if pending.wav.len() < 44 + crate::wav::RATE as usize / 5 * 2 {
            return false;
        }
        self.pending = Some(pending);
        self.in_flight = true;
        true
    }
    fn complete(&mut self, consume: bool, gestures: &mut Gestures) {
        self.in_flight = false;
        if consume {
            self.pending = None;
        }
        // A terminal rejection consumes audio; retryable failures keep it independent of readiness.
        gestures.ready();
    }
    fn retry(&mut self, gestures: &mut Gestures) -> (Effect, Option<Pending>) {
        if self.in_flight || !gestures.is_ready() {
            return (Effect::Busy, None);
        }
        let pending = self.pending.clone();
        if pending.is_some() {
            self.in_flight = true;
            gestures.busy();
        }
        (Effect::None, pending)
    }
}
fn upload(uploader: Uploader, pending: Pending, tx: mpsc::Sender<Result<String>>) {
    tokio::spawn(async move {
        let result = uploader.transcribe(&pending.wav, &pending.mode).await;
        let _ = tx.send(result).await;
    });
}
fn start_insertion<F>(tx: mpsc::Sender<Result<()>>, work: F)
where
    F: FnOnce(insert::Deadline) -> Result<()> + Send + 'static,
{
    tokio::spawn(async move {
        let _ = tx.send(insert::bounded_insertion(work).await).await;
    });
}
pub async fn run(c: Config, test_insertion: bool) -> Result<()> {
    let uploader = Uploader::new(&c)?;
    let (action_tx, mut actions) = mpsc::channel(16);
    let notifications = feedback::Notifications::new(action_tx);
    let (event_tx, mut events) = mpsc::channel(64);
    let backend = if c.device == "test" && c.insert == "print" {
        hotkeys::Backend::bind_test(&c.shortcuts, event_tx).await?
    } else {
        hotkeys::Backend::bind(&c.shortcuts, event_tx).await?
    };
    // Wait for Handy before creating a virtual keyboard; startup must not churn
    // libinput devices on the live desktop while Handy owns the default chords.
    let keyboard = if c.insert == "paste" {
        Some(Arc::new(Mutex::new(
            tokio::task::spawn_blocking(insert::Keyboard::new).await??,
        )))
    } else {
        None
    };
    let (insert_tx, mut inserted) = mpsc::channel::<Result<()>>(1);
    let mut inserting = false;
    let overlay = overlay::Overlay::new(c.overlay, c.overlay_output.clone());
    let (done_tx, mut done) = mpsc::channel::<Result<String>>(1);
    let mut gestures = Gestures::new(c.tap_ms);
    let clock = Instant::now();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut recording: Option<audio::Recording> = None;
    let mut mode = String::new();
    let mut uploads = Uploads::default();
    let outcome:Result<()>=async {
        loop {
            let effect=tokio::select! {
                event=events.recv()=> {
                    let e=event.context("shortcut backend disconnected")??;
                    let now=e.received.saturating_duration_since(clock).as_millis() as u64;
                    eprintln!("{now} ms {} {}",if e.pressed {"Pressed"} else {"Released"},e.key);
                    if e.pressed {
                        let binding=c.shortcuts.iter().find(|s|s.key==e.key).context("unknown shortcut")?;
                        gestures.press(&e.key,&binding.mode,now)
                    } else {
                        let effect=gestures.release(&e.key,now);
                        if gestures.is_toggle() {
                            overlay.set(overlay::State::Recording{mode:mode.clone(),toggle:true});
                        }
                        effect
                    }
                },
                _=tick.tick()=>gestures.tick(clock.elapsed().as_millis() as u64),
                result=done.recv(),if uploads.in_flight=> {
                    let result=result.context("upload worker disconnected")?;
                    uploads.complete(result.is_ok() || result.as_ref().err().is_some_and(crate::upload::terminal),&mut gestures);
                    match result {
                        Ok(text)=> {
                            // A successful response consumes the audio even if insertion fails.
                            notifications.close();
                            eprintln!("upload complete");
                            if text.is_empty() {
                                overlay.set(overlay::State::NothingHeard);
                                audio::sound(c.sounds,350.0);
                            } else if let Some(k)=&keyboard {
                                gestures.busy();
                                inserting=true;
                                let keyboard=k.clone();
                                let delay=c.restore_delay_ms;
                                start_insertion(insert_tx.clone(),move |deadline| {
                                    let mut k=keyboard.try_lock().map_err(|_|anyhow::anyhow!("insertion keyboard is still busy"))?;
                                    insert::insert(&text,&mut k,delay,deadline)
                                });
                            } else if test_insertion {
                                gestures.busy();
                                inserting=true;
                                let delay=c.restore_delay_ms;
                                start_insertion(insert_tx.clone(),move |deadline|insert::insert_print(text,delay,deadline));
                            } else {
                                println!("{text}");
                                overlay.set(overlay::State::Done);
                            }
                        },
                        Err(e)=> {
                            overlay.set(overlay::State::Failed);
                            let message=c.redact(&e.to_string());
                            let retry=uploads.pending.is_some();
                            eprintln!("upload failed: {message}; {}",if retry {"audio retained for Retry or Discard"} else {"terminal rejection; audio discarded"});
                            audio::sound(c.sounds,200.0);
                            notifications.error(&message,retry);
                        },
                    }
                    Effect::None
                },
                result=inserted.recv(),if inserting=> {
                    inserting=false;
                    gestures.ready();
                    match result.context("insertion worker disconnected")? {
                        Ok(())=>overlay.set(overlay::State::Done),
                        Err(e)=> {
                            overlay.set(overlay::State::Failed);
                            let message=c.redact(&e.to_string());
                            eprintln!("insertion failed: {message}");
                            notifications.error(&message,false);
                            audio::sound(c.sounds,200.0);
                        },
                    }
                    Effect::None
                },
                Some(action)=actions.recv()=> {
                    if uploads.in_flight || inserting {
                        Effect::Busy
                    } else {
                        match action {
                            feedback::Action::Retry=> {
                                let (effect,pending)=uploads.retry(&mut gestures);
                                if let Some(p)=pending {
                                    overlay.set(overlay::State::Processing);
                                    upload(uploader.clone(),p,done_tx.clone());
                                }
                                effect
                            },
                            feedback::Action::Discard=> {
                                if uploads.pending.is_some() {
                                    uploads.discard();
                                    overlay.set(overlay::State::Idle);
                                    notifications.close();
                                }
                                Effect::None
                            },
                        }
                    }
                },
                _=hotkeys::shutdown()=>break,
            };
            match effect {
                Effect::Start(m)=> {
                    match audio::Recording::start(c.input_device.as_deref(),overlay.level.clone()) {
                    Ok(r)=> {
                        uploads.new_recording();
                        notifications.close();
                        // Queued actions belong to the discarded recording's notification.
                        while actions.try_recv().is_ok() {}
                        recording=Some(r);
                        mode=m;
                        overlay.set(overlay::State::Recording{mode:mode.clone(),toggle:false});
                        audio::sound(c.sounds,880.0);
                    },
                    Err(e)=> {
                        overlay.set(overlay::State::Failed);
                        gestures.ready();
                        let message=c.redact(&e.to_string());
                        eprintln!("capture: {message}");
                        audio::sound(c.sounds,200.0);
                        notifications.error(&message,uploads.pending.is_some());
                    },
                    }
                },
                Effect::Stop=> {
                    overlay.set(overlay::State::Processing);
                    audio::sound(c.sounds,660.0);
                    if let Some(r)=recording.take() {
                        match r.finish() {
                            Ok(wav)=> {
                                let p=Pending{wav,mode:mode.clone()};
                                if uploads.begin(p.clone()) {
                                    eprintln!("uploading {} WAV bytes, mode={mode}",p.wav.len());
                                    upload(uploader.clone(),p,done_tx.clone());
                                } else {
                                    eprintln!("nothing heard: recording shorter than 200 ms; not uploaded");
                                    gestures.ready();
                                    overlay.set(overlay::State::NothingHeard);
                                    audio::sound(c.sounds,350.0);
                                }
                            },
                            Err(e)=> {
                                overlay.set(overlay::State::Failed);
                                gestures.ready();
                                let message=c.redact(&e.to_string());
                                notifications.error(&message,false);
                                audio::sound(c.sounds,200.0);
                            },
                        }
                    }
                },
                Effect::Busy=>audio::sound(c.sounds,300.0),
                Effect::None=>{},
            }
        }
        Ok(())
    }.await;
    backend.close().await;
    notifications.close();
    outcome
}
#[cfg(test)]
mod tests {
    use super::*;
    fn pending(byte: u8) -> Pending {
        Pending {
            wav: crate::wav::encode(&vec![byte as i16; 3200]),
            mode: "raw".into(),
        }
    }
    #[test]
    fn empty_and_short_recordings_never_reach_upload() {
        for samples in [0, 1, 3199] {
            let mut uploads = Uploads::default();
            let p = Pending {
                wav: crate::wav::encode(&vec![0; samples]),
                mode: "raw".into(),
            };
            assert!(!uploads.begin(p));
            assert!(!uploads.in_flight);
            assert!(uploads.pending.is_none());
        }
        let mut uploads = Uploads::default();
        assert!(uploads.begin(pending(1))); // Exactly 200 ms is uploadable.
    }
    #[tokio::test]
    async fn stalled_clipboard_does_not_queue_hotkeys_and_next_press_starts() {
        use std::{io::Write, os::unix::net::UnixStream};
        let (mut reader, mut owner) = UnixStream::pair().unwrap();
        owner.write_all(b"owner never sends EOF").unwrap();
        let mut gestures = Gestures::new(300);
        gestures.busy();
        let (tx, mut done) = mpsc::channel(1);
        start_insertion(tx, move |_| {
            insert::read_pipe(
                &mut reader,
                &mut vec![],
                Instant::now() + Duration::from_millis(250),
            )
        });
        // This branch runs while the insertion worker is blocked waiting for EOF.
        tokio::select! {
            _=tokio::time::sleep(Duration::from_millis(30))=>assert_eq!(gestures.press("a","raw",30),Effect::Busy),
            _=done.recv()=>panic!("clipboard did not stall"),
        }
        let result = tokio::time::timeout(Duration::from_millis(500), done.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        gestures.ready(); // Same completion transition as the real daemon.
        assert_eq!(gestures.press("a", "raw", 300), Effect::Start("raw".into()));
        drop(owner);
    }
    #[test]
    fn terminal_rejection_discards_audio_without_retry() {
        let mut uploads = Uploads::default();
        let mut gestures = Gestures::new(300);
        uploads.begin(pending(1));
        uploads.complete(true, &mut gestures);
        assert!(gestures.is_ready());
        assert!(uploads.retry(&mut gestures).1.is_none());
    }
    #[test]
    fn failed_upload_is_ready_and_new_recording_replaces_audio() {
        let mut gestures = Gestures::new(300);
        let mut uploads = Uploads::default();
        gestures.busy();
        uploads.begin(pending(1));
        uploads.complete(false, &mut gestures);
        assert!(gestures.is_ready());
        assert!(!uploads.in_flight);
        assert_eq!(uploads.pending.as_ref().unwrap().wav, pending(1).wav);
        // No notification-close event participates in dictation state.
        assert_eq!(
            gestures.press("a", "clean", 500),
            Effect::Start("clean".into())
        );
        uploads.new_recording();
        assert!(uploads.pending.is_none());
        uploads.begin(pending(2));
        assert_eq!(uploads.pending.as_ref().unwrap().wav, pending(2).wav);
    }
    #[test]
    fn retry_is_busy_during_hold_toggle_and_upload() {
        for toggle in [false, true] {
            let mut gestures = Gestures::new(300);
            let mut uploads = Uploads::default();
            uploads.begin(pending(1));
            uploads.complete(false, &mut gestures);
            gestures.press("a", "raw", 0);
            if toggle {
                gestures.release("a", 10);
            }
            let (effect, retry) = uploads.retry(&mut gestures);
            assert_eq!(effect, Effect::Busy);
            assert!(retry.is_none());
            assert!(!uploads.in_flight);
            assert_eq!(gestures.tick(600_000), Effect::Stop);
            gestures.ready();
            let (effect, retry) = uploads.retry(&mut gestures);
            assert_eq!(effect, Effect::None);
            assert_eq!(retry.unwrap().wav, pending(1).wav);
            assert!(uploads.in_flight);
            assert_eq!(uploads.retry(&mut gestures).0, Effect::Busy);
            assert_eq!(gestures.press("b", "clean", 600_001), Effect::Busy);
        }
    }
    #[test]
    fn retry_failure_retains_audio_success_and_discard_consume_it() {
        let mut gestures = Gestures::new(300);
        let mut uploads = Uploads::default();
        uploads.begin(pending(1));
        uploads.complete(false, &mut gestures);
        assert!(uploads.retry(&mut gestures).1.is_some());
        uploads.complete(false, &mut gestures);
        assert!(gestures.is_ready());
        assert!(uploads.retry(&mut gestures).1.is_some());
        uploads.complete(true, &mut gestures);
        assert!(uploads.pending.is_none());
        assert!(gestures.is_ready());
        uploads.begin(pending(2));
        uploads.complete(false, &mut gestures);
        uploads.discard();
        assert!(uploads.pending.is_none());
        assert!(gestures.is_ready());
        assert!(uploads.retry(&mut gestures).1.is_none());
    }
}
