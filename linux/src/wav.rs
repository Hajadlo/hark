pub const RATE: u32 = 16_000;
pub const MAX_SAMPLES: usize = RATE as usize * 600;
pub fn pcm(sample: f32) -> i16 {
    if !sample.is_finite() {
        return 0;
    }
    if sample <= -1.0 {
        i16::MIN
    } else {
        (sample.min(1.0) * i16::MAX as f32).round() as i16
    }
}
pub fn encode(samples: &[i16]) -> Vec<u8> {
    let samples = &samples[..samples.len().min(MAX_SAMPLES)];
    let len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + len as usize);
    out.extend(b"RIFF");
    out.extend((36 + len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(1u16.to_le_bytes());
    out.extend(RATE.to_le_bytes());
    out.extend((RATE * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(len.to_le_bytes());
    for s in samples {
        out.extend(s.to_le_bytes());
    }
    out
}
// Weighted box averaging resamples capture without retaining native-rate audio.
pub struct Resampler {
    rate: f64,
    remaining: f64,
    sum: f64,
    weight: f64,
    pub samples: Vec<i16>,
}
impl Resampler {
    pub fn new(rate: u32) -> Self {
        let ratio = rate as f64 / RATE as f64;
        Self {
            rate: ratio,
            remaining: ratio,
            sum: 0.0,
            weight: 0.0,
            samples: Vec::new(),
        }
    }
    pub fn push(&mut self, s: f32) {
        let mut left = 1.0f64;
        while left > 1e-9 && self.samples.len() < MAX_SAMPLES {
            let n = left.min(self.remaining);
            self.sum += s as f64 * n;
            self.weight += n;
            self.remaining -= n;
            left -= n;
            if self.remaining < 1e-9 {
                self.samples.push(pcm((self.sum / self.weight) as f32));
                self.sum = 0.0;
                self.weight = 0.0;
                self.remaining = self.rate;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn header_bytes_and_conversion() {
        let b = encode(&[pcm(-1.0), pcm(0.0), pcm(1.0)]);
        assert_eq!(&b[..4], b"RIFF");
        assert_eq!(&b[8..16], b"WAVEfmt ");
        assert_eq!(&b[20..24], &[1, 0, 1, 0]);
        assert_eq!(&b[24..28], &16000u32.to_le_bytes());
        assert_eq!(&b[28..36], &[0, 125, 0, 0, 2, 0, 16, 0]);
        assert_eq!(&b[36..44], &[100, 97, 116, 97, 6, 0, 0, 0]);
        assert_eq!(&b[44..], &[0, 128, 0, 0, 255, 127]);
        assert_eq!(pcm(f32::NAN), 0);
        assert_eq!(pcm(9.0), 32767);
    }
    #[test]
    fn ten_minute_cap() {
        let b = encode(&vec![0; MAX_SAMPLES + 100]);
        assert_eq!(b.len(), 44 + MAX_SAMPLES * 2);
        assert_eq!(
            u32::from_le_bytes(b[4..8].try_into().unwrap()),
            36 + (MAX_SAMPLES * 2) as u32
        );
    }
    #[test]
    fn resampling_and_downmix_precision() {
        let mut r = Resampler::new(48000);
        for _ in 0..48000 {
            r.push(0.5);
        }
        assert_eq!(r.samples.len(), 16000);
        assert!(r.samples.iter().all(|s| *s == 16384));
        let mut r = Resampler::new(44100);
        for _ in 0..44100 {
            r.push(0.0);
        }
        assert_eq!(r.samples.len(), 16000);
    }
}

/// CLI files use the same wire format and cap as microphone recordings.
pub fn from_wav(bytes: &[u8]) -> anyhow::Result<Vec<u8>> {
    use anyhow::{Context, ensure};
    let mut reader =
        hound::WavReader::new(std::io::Cursor::new(bytes)).context("unreadable WAV")?;
    let spec = reader.spec();
    ensure!(
        spec.channels > 0 && spec.sample_rate > 0,
        "invalid WAV format"
    );
    ensure!(
        reader.duration() as u64 <= spec.sample_rate as u64 * 600,
        "WAV exceeds ten minutes"
    );
    if spec.channels == 1
        && spec.sample_rate == RATE
        && spec.sample_format == hound::SampleFormat::Int
        && spec.bits_per_sample == 16
    {
        return Ok(encode(
            &reader.samples::<i16>().collect::<Result<Vec<_>, _>>()?,
        ));
    }
    let mut resampler = Resampler::new(spec.sample_rate);
    let mut frame = Vec::with_capacity(spec.channels as usize);
    let mut push = |sample: f32| {
        frame.push(sample);
        if frame.len() == spec.channels as usize {
            resampler.push(frame.iter().sum::<f32>() / spec.channels as f32);
            frame.clear();
        }
    };
    match spec.sample_format {
        hound::SampleFormat::Float => {
            for sample in reader.samples::<f32>() {
                push(sample?);
            }
        }
        hound::SampleFormat::Int => {
            let scale = 2.0f32.powi(spec.bits_per_sample as i32 - 1);
            for sample in reader.samples::<i32>() {
                push(sample? as f32 / scale);
            }
        }
    }
    ensure!(frame.is_empty(), "incomplete WAV channel frame");
    Ok(encode(&resampler.samples))
}
#[cfg(test)]
mod file_tests {
    use super::*;
    #[test]
    fn pcm16_roundtrip_and_invalid_file() {
        let wav = encode(&[i16::MIN, 0, i16::MAX]);
        assert_eq!(from_wav(&wav).unwrap(), wav);
        assert!(from_wav(b"not a WAV").is_err());
    }
    #[test]
    fn stereo_float_48k_to_mono_16k() {
        let mut cursor = std::io::Cursor::new(Vec::new());
        {
            let mut w = hound::WavWriter::new(
                &mut cursor,
                hound::WavSpec {
                    channels: 2,
                    sample_rate: 48000,
                    bits_per_sample: 32,
                    sample_format: hound::SampleFormat::Float,
                },
            )
            .unwrap();
            for _ in 0..480 {
                w.write_sample(1.0f32).unwrap();
                w.write_sample(0.0f32).unwrap();
            }
            w.finalize().unwrap();
        }
        let out = from_wav(cursor.get_ref()).unwrap();
        assert_eq!(out.len(), 44 + 160 * 2);
        assert_eq!(&out[44..46], &16384i16.to_le_bytes());
    }
}
