use crate::{
    overlay::{Level, Smoother},
    wav::Resampler,
};
use anyhow::{Context, Result, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};
pub struct Recording {
    stream: cpal::Stream,
    data: Arc<Mutex<Resampler>>,
    error: Arc<Mutex<Option<String>>>,
}
impl Recording {
    pub fn start(input: Option<&str>, level: Arc<Level>) -> Result<Self> {
        level.publish(0.0);
        let host = cpal::default_host();
        let device = if let Some(name) = input {
            host.input_devices()?
                .find(|d| d.name().ok().as_deref() == Some(name))
                .with_context(|| format!("input device unavailable: {name}"))?
        } else {
            host.default_input_device()
                .context("no default input device")?
        };
        let config = device.default_input_config()?;
        let channels = config.channels() as usize;
        let rate = config.sample_rate().0;
        let data = Arc::new(Mutex::new(Resampler::new(rate)));
        let error = Arc::new(Mutex::new(None));
        let cfg: cpal::StreamConfig = config.clone().into();
        let stream = match config.sample_format() {
            cpal::SampleFormat::F32 => {
                build::<f32>(&device, &cfg, channels, data.clone(), error.clone(), level)?
            }
            cpal::SampleFormat::I16 => {
                build::<i16>(&device, &cfg, channels, data.clone(), error.clone(), level)?
            }
            cpal::SampleFormat::U16 => {
                build::<u16>(&device, &cfg, channels, data.clone(), error.clone(), level)?
            }
            format => bail!("unsupported capture sample format: {format}"),
        };
        stream.play()?;
        Ok(Self {
            stream,
            data,
            error,
        })
    }
    pub fn finish(self) -> Result<Vec<u8>> {
        drop(self.stream);
        if let Some(error) = self.error.lock().unwrap().take() {
            bail!("audio capture failed: {error}");
        }
        let data = self.data.lock().unwrap();
        Ok(crate::wav::encode(&data.samples))
    }
}
fn build<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    channels: usize,
    data: Arc<Mutex<Resampler>>,
    error: Arc<Mutex<Option<String>>>,
    level: Arc<Level>,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let mut smoother = Smoother::default();
    let rate = cfg.sample_rate.0 as f32;
    Ok(device.build_input_stream(
        cfg,
        move |samples: &[T], _| {
            let mut data = data.lock().unwrap();
            let mut square_sum = 0.0;
            let mut frames = 0;
            for frame in samples.chunks_exact(channels) {
                let mono = frame
                    .iter()
                    .map(|s| <f32 as cpal::FromSample<T>>::from_sample_(*s))
                    .sum::<f32>()
                    / channels as f32;
                data.push(mono);
                square_sum += mono * mono;
                frames += 1;
            }
            if frames > 0 {
                let rms = (square_sum / frames as f32).sqrt();
                level.publish(smoother.update(rms, frames as f32 / rate));
            }
        },
        move |e| {
            *error.lock().unwrap() = Some(e.to_string());
        },
        None,
    )?)
}
pub fn devices() -> Result<Vec<String>> {
    Ok(cpal::default_host()
        .input_devices()?
        .filter_map(|d| d.name().ok())
        .collect())
}
pub fn sound(enabled: bool, frequency: f32) {
    if !enabled {
        return;
    }
    std::thread::spawn(move || {
        let samples: Vec<i16> = (0..1600)
            .map(|n| {
                crate::wav::pcm(
                    (n as f32 * frequency * std::f32::consts::TAU / 16000.0).sin()
                        * 0.08
                        * (1.0 - n as f32 / 1600.0),
                )
            })
            .collect();
        // PipeWire's player follows the default sink without changing it.
        use std::io::Write;
        if let Ok(mut child) = std::process::Command::new("pw-play")
            .args(["--raw", "--rate=16000", "--channels=1", "--format=s16", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            if let Some(mut pipe) = child.stdin.take() {
                let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
                let _ = pipe.write_all(&bytes);
            }
            let _ = child.wait();
        }
    });
}
