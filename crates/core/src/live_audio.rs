use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::{FftFixedIn, Resampler};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use tokio::sync::mpsc as async_mpsc;
use tokio_util::sync::CancellationToken;

pub const SAMPLE_RATE: usize = 24_000;
pub type AudioReceiver = async_mpsc::Receiver<std::result::Result<Vec<u8>, String>>;

pub struct LiveRecorder {
    stop: CancellationToken,
}
impl LiveRecorder {
    pub fn start(
        device_name: Option<String>,
        level: Arc<dyn Fn(f32) + Send + Sync>,
    ) -> Result<(Self, AudioReceiver)> {
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        // No audio files or unbounded buffers. A stalled upload ends capture
        // visibly instead of silently dropping speech or retaining a recording.
        let (output, receiver) = async_mpsc::channel(128);
        thread::spawn(move || {
            let (input_tx, input_rx) = mpsc::sync_channel::<Vec<f32>>(32);
            let failed = Arc::new(AtomicBool::new(false));
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
                let channels = supported.channels() as usize;
                let rate = supported.sample_rate().0 as usize;
                let converter = PcmConverter::new(rate)?;
                macro_rules! build {
                    ($ty:ty, $convert:expr) => {{
                        let tx = input_tx.clone();
                        let overflow = failed.clone();
                        let disconnected = failed.clone();
                        device.build_input_stream(
                            &supported.config(),
                            move |data: &[$ty], _| {
                                if overflow.load(Ordering::Relaxed) {
                                    return;
                                }
                                // Bound callback allocations even for unusual device buffers.
                                if data.len() / channels > rate {
                                    overflow.store(true, Ordering::Relaxed);
                                    return;
                                }
                                let mono: Vec<f32> = data
                                    .chunks_exact(channels)
                                    .map(|frame| {
                                        frame.iter().map($convert).sum::<f32>() / channels as f32
                                    })
                                    .collect();
                                if !mono.is_empty() {
                                    if tx.try_send(mono).is_err() {
                                        overflow.store(true, Ordering::Relaxed);
                                    }
                                }
                            },
                            move |_| {
                                disconnected.store(true, Ordering::Relaxed);
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
                Ok((stream, converter))
            };
            let (stream, mut converter) = match setup() {
                Ok(parts) => {
                    let _ = ready_tx.send(Ok(()));
                    parts
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e.to_string()));
                    return;
                }
            };
            drop(input_tx);
            let publish = |bytes: Vec<u8>| -> Result<()> {
                if !bytes.is_empty() {
                    output.try_send(Ok(bytes)).map_err(|_| {
                        anyhow::anyhow!(
                            "Live audio could not keep up with the connection; capture stopped"
                        )
                    })?;
                }
                Ok(())
            };
            let mut meter_frames = 0;
            let operation = (|| -> Result<()> {
                while !stopped.is_cancelled() {
                    if failed.load(Ordering::Relaxed) {
                        bail!("Microphone disconnected or its audio buffer overflowed");
                    }
                    match input_rx.recv_timeout(Duration::from_millis(20)) {
                        Ok(samples) => {
                            meter_frames += samples.len();
                            if meter_frames >= converter.input_rate / 20 {
                                level(
                                    (samples.iter().map(|s| s * s).sum::<f32>()
                                        / samples.len() as f32)
                                        .sqrt(),
                                );
                                meter_frames = 0;
                            }
                            publish(converter.push(&samples)?)?;
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => (),
                        Err(_) => bail!("Microphone disconnected"),
                    }
                }
                Ok(())
            })();
            drop(stream);
            let result = operation.and_then(|_| {
                if failed.load(Ordering::Relaxed) {
                    bail!("Microphone disconnected or its audio buffer overflowed");
                }
                for samples in input_rx.try_iter() {
                    publish(converter.push(&samples)?)?;
                }
                publish(converter.finish()?)
            });
            if let Err(e) = result {
                let _ = output.blocking_send(Err(e.to_string()));
            }
        });
        ready_rx
            .recv()
            .context("Microphone unavailable")?
            .map_err(anyhow::Error::msg)?;
        Ok((Self { stop }, receiver))
    }
}
impl Drop for LiveRecorder {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

struct PcmConverter {
    resampler: FftFixedIn<f32>,
    pending: Vec<f32>,
    input_rate: usize,
    input_frames: usize,
    output_frames: usize,
    skip: usize,
}
impl PcmConverter {
    fn new(rate: usize) -> Result<Self> {
        let resampler = FftFixedIn::new(rate, SAMPLE_RATE, 1024, 2, 1)?;
        let skip = resampler.output_delay();
        Ok(Self {
            resampler,
            pending: Vec::new(),
            input_rate: rate,
            input_frames: 0,
            output_frames: 0,
            skip,
        })
    }
    fn push(&mut self, samples: &[f32]) -> Result<Vec<u8>> {
        self.input_frames += samples.len();
        self.pending.extend_from_slice(samples);
        let mut bytes = Vec::new();
        while self.pending.len() >= self.resampler.input_frames_next() {
            let n = self.resampler.input_frames_next();
            let input: Vec<f32> = self.pending.drain(..n).collect();
            let out = self.resampler.process(&[input], None)?;
            self.encode(&out[0], &mut bytes);
        }
        Ok(bytes)
    }
    fn encode(&mut self, samples: &[f32], bytes: &mut Vec<u8>) {
        let skip = self.skip.min(samples.len());
        self.skip -= skip;
        let remaining = self.input_frames * SAMPLE_RATE / self.input_rate - self.output_frames;
        for s in samples.iter().skip(skip).take(remaining) {
            bytes.extend_from_slice(&((s.clamp(-1., 1.) * 32767.) as i16).to_le_bytes());
            self.output_frames += 1;
        }
    }
    fn finish(&mut self) -> Result<Vec<u8>> {
        let target = self.input_frames * SAMPLE_RATE / self.input_rate;
        let mut bytes = Vec::new();
        while self.output_frames < target {
            self.pending.resize(self.resampler.input_frames_next(), 0.);
            let out = self
                .resampler
                .process(&[std::mem::take(&mut self.pending)], None)?;
            self.encode(&out[0], &mut bytes);
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resampling_keeps_duration_and_tail_across_callback_boundaries() {
        for rate in [16_000, 24_000, 44_100, 48_000, 96_000] {
            let samples: Vec<_> = (0..rate + 317)
                .map(|n| (n as f32 * 1000. * std::f32::consts::TAU / rate as f32).sin() * 0.5)
                .collect();
            let convert = |chunk| {
                let mut c = PcmConverter::new(rate).unwrap();
                let mut bytes = Vec::new();
                for s in samples.chunks(chunk) {
                    bytes.extend(c.push(s).unwrap());
                }
                bytes.extend(c.finish().unwrap());
                bytes
            };
            let a = convert(137);
            assert_eq!(a, convert(2048));
            assert_eq!(a.len(), samples.len() * SAMPLE_RATE / rate * 2);
            let tail: Vec<_> = a[a.len() - 480..]
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.)
                .collect();
            assert!(tail.iter().map(|s| s * s).sum::<f32>() / tail.len() as f32 > 0.05);
        }
    }
}
