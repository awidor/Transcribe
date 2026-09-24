use crate::provider::Audio;
use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    io::Cursor,
    sync::{mpsc, Arc, Mutex},
    thread,
};

pub const MAX_SECONDS: u64 = 300;

/// A recording with nothing to transcribe. It is an outcome, not a failure.
#[derive(Debug)]
pub struct NoSpeech;
impl std::fmt::Display for NoSpeech {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("No speech detected")
    }
}
impl std::error::Error for NoSpeech {}
pub struct Recorder {
    stop: mpsc::Sender<bool>,
    result: Option<thread::JoinHandle<Result<(Audio, f64)>>>,
}
pub fn devices() -> Result<Vec<String>> {
    Ok(cpal::default_host()
        .input_devices()?
        .filter_map(|d| d.name().ok())
        .collect())
}
impl Recorder {
    pub fn start(
        device_name: Option<String>,
        level: Arc<dyn Fn(f32) + Send + Sync>,
    ) -> Result<Self> {
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        let (stop, stop_rx) = mpsc::channel();
        let result = thread::spawn(move || {
            let setup = || -> Result<_> {
                let host = cpal::default_host();
                let device = match device_name {
                    Some(name) => host
                        .input_devices()?
                        .find(|d| d.name().ok().as_deref() == Some(&name))
                        .context("Microphone unavailable")?,
                    None => host
                        .default_input_device()
                        .context("Microphone unavailable")?,
                };
                let supported = device
                    .default_input_config()
                    .context("Microphone unavailable")?;
                let rate = supported.sample_rate().0;
                let channels = supported.channels() as usize;
                let samples = Arc::new(Mutex::new(Vec::<i16>::new()));
                let stream_error = Arc::new(Mutex::new(None::<String>));
                let config = supported.config();
                macro_rules! build {
                    ($ty:ty, $convert:expr) => {{
                        let data = samples.clone();
                        let l = level.clone();
                        let error = stream_error.clone();
                        let mut ticks = 0u32;
                        device.build_input_stream(
                            &config,
                            move |input: &[$ty], _| {
                                let Ok(mut out) = data.lock() else {
                                    return;
                                };
                                let limit = rate as usize * MAX_SECONDS as usize;
                                let mut energy = 0f32;
                                let mut count = 0;
                                for frame in input.chunks_exact(channels) {
                                    let sample =
                                        frame.iter().map($convert).sum::<f32>() / channels as f32;
                                    energy += sample * sample;
                                    count += 1;
                                    if out.len() < limit {
                                        out.push((sample.clamp(-1., 1.) * i16::MAX as f32) as i16);
                                    }
                                }
                                ticks += input.len() as u32 / channels as u32;
                                if ticks > rate / 20 {
                                    l((energy / count.max(1) as f32).sqrt());
                                    ticks = 0;
                                }
                            },
                            move |_| {
                                if let Ok(mut e) = error.lock() {
                                    *e = Some("Microphone disconnected".into());
                                }
                            },
                            None,
                        )?
                    }};
                }
                let stream = match supported.sample_format() {
                    cpal::SampleFormat::F32 => build!(f32, |v: &f32| *v),
                    cpal::SampleFormat::I16 => build!(i16, |v: &i16| *v as f32 / 32768.),
                    cpal::SampleFormat::U16 => build!(u16, |v: &u16| (*v as f32 - 32768.) / 32768.),
                    _ => bail!("Microphone format unsupported"),
                };
                stream.play().context("Microphone permission required")?;
                Ok((stream, samples, stream_error, rate))
            };
            let (stream, samples, stream_error, rate) = match setup() {
                Ok(parts) => {
                    let _ = ready_tx.send(Ok(()));
                    parts
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return Err(e);
                }
            };
            let keep = stop_rx.recv().unwrap_or(false);
            drop(stream);
            if !keep {
                bail!("Cancelled");
            }
            if let Some(error) = stream_error.lock().unwrap().take() {
                bail!(error);
            }
            let samples = samples.lock().unwrap();
            anyhow::ensure!(samples.len() > rate as usize / 5, NoSpeech);
            let seconds = samples.len() as f64 / rate as f64;
            let mut wav = Cursor::new(Vec::new());
            {
                let mut writer = hound::WavWriter::new(
                    &mut wav,
                    hound::WavSpec {
                        channels: 1,
                        sample_rate: rate,
                        bits_per_sample: 16,
                        sample_format: hound::SampleFormat::Int,
                    },
                )?;
                for &s in samples.iter() {
                    writer.write_sample(s)?;
                }
                writer.finalize()?;
            }
            Ok((
                Audio {
                    bytes: wav.into_inner(),
                    format: "wav".into(),
                },
                seconds,
            ))
        });
        ready_rx
            .recv()
            .context("Microphone unavailable")?
            .map_err(anyhow::Error::msg)?;
        Ok(Self {
            stop,
            result: Some(result),
        })
    }
    pub fn finish(mut self) -> Result<(Audio, f64)> {
        self.stop.send(true)?;
        self.result
            .take()
            .unwrap()
            .join()
            .map_err(|_| anyhow::anyhow!("Recording failed"))?
    }
}
impl Drop for Recorder {
    fn drop(&mut self) {
        let _ = self.stop.send(false);
    }
}
