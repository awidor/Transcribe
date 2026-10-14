use anyhow::{bail, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rubato::{FftFixedIn, Resampler};
use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    thread,
    time::Duration,
};
use tokio::sync::mpsc as async_mpsc;
use tokio_util::sync::CancellationToken;

pub const MAX_SECONDS: u64 = 300;
pub const SAMPLE_RATE: usize = 24_000;
/// Mono 16-bit PCM packets at `SAMPLE_RATE`, or the error that ended them.
pub type AudioReceiver = async_mpsc::Receiver<std::result::Result<Vec<u8>, String>>;

/// A recording with nothing to transcribe. It is an outcome, not a failure.
#[derive(Debug)]
pub struct NoSpeech;
impl std::fmt::Display for NoSpeech {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("No speech detected")
    }
}
impl std::error::Error for NoSpeech {}

/// A second-order filter (RBJ biquad, Q = 1/√2). The level meter listens
/// through a high-pass and a low-pass pair, so rumble and hiss outside the
/// voice band do not move the widget's bars. Recordings are unfiltered.
struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}
impl Biquad {
    fn new(rate: u32, frequency: f32, high_pass: bool) -> Self {
        let frequency = frequency.min(rate as f32 * 0.45);
        let (sin, cos) = (std::f32::consts::TAU * frequency / rate as f32).sin_cos();
        let alpha = sin / std::f32::consts::SQRT_2;
        let a0 = 1. + alpha;
        let (edge, middle) = if high_pass {
            ((1. + cos) / 2., -(1. + cos))
        } else {
            ((1. - cos) / 2., 1. - cos)
        };
        Self {
            b: [edge / a0, middle / a0, edge / a0],
            a: [-2. * cos / a0, (1. - alpha) / a0],
            z: [0.; 2],
        }
    }
    fn process(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

pub fn devices() -> Result<Vec<String>> {
    Ok(cpal::default_host()
        .input_devices()?
        .filter_map(|d| d.name().ok())
        .collect())
}

/// Streams the microphone as PCM packets until dropped. Dropping it flushes
/// the remaining audio and then closes the receiver.
pub struct Recorder {
    stop: CancellationToken,
}
impl Recorder {
    pub fn start(
        device_name: Option<String>,
        level: Arc<dyn Fn(f32) + Send + Sync>,
    ) -> Result<(Self, AudioReceiver)> {
        let stop = CancellationToken::new();
        let stopped = stop.clone();
        let (ready_tx, ready_rx) = mpsc::sync_channel(1);
        // About twenty seconds of packets: speech captured while the
        // connection opens waits here. No audio files or unbounded buffers.
        let (output, receiver) = async_mpsc::channel(1024);
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
                                if !mono.is_empty() && tx.try_send(mono).is_err() {
                                    overflow.store(true, Ordering::Relaxed);
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
                        anyhow::anyhow!("The connection could not keep up; recording stopped")
                    })?;
                }
                Ok(())
            };
            let rate = converter.input_rate as u32;
            // The level covers every frame since the last report, about 30
            // times a second.
            let (mut rumble, mut hiss) = (
                Biquad::new(rate, 200., true),
                Biquad::new(rate, 4000., false),
            );
            let (mut frames, mut energy) = (0u32, 0f32);
            let operation = (|| -> Result<()> {
                while !stopped.is_cancelled() {
                    if failed.load(Ordering::Relaxed) {
                        bail!("Microphone disconnected or its audio buffer overflowed");
                    }
                    match input_rx.recv_timeout(Duration::from_millis(20)) {
                        Ok(samples) => {
                            for &sample in &samples {
                                let voice = hiss.process(rumble.process(sample));
                                energy += voice * voice;
                                frames += 1;
                            }
                            if frames > rate / 30 {
                                level((energy / frames as f32).sqrt());
                                (frames, energy) = (0, 0.);
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
impl Drop for Recorder {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

/// Opens a WAV, MP3 or FLAC file and decodes it as PCM packets on a
/// background thread. Unreadable files fail here, before anything is sent.
pub fn decode(path: &Path) -> Result<AudioReceiver> {
    use symphonia::core::{
        codecs::audio::AudioDecoderOptions,
        errors::Error,
        formats::{probe::Hint, TrackType},
        io::MediaSourceStream,
    };
    let unsupported = || anyhow::anyhow!("Audio format unsupported");
    let file = std::fs::File::open(path).context("Audio could not be read")?;
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(extension);
    }
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            MediaSourceStream::new(Box::new(file), Default::default()),
            Default::default(),
            Default::default(),
        )
        .map_err(|_| unsupported())?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(unsupported)?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(unsupported)?;
    let rate = params.sample_rate.ok_or_else(unsupported)? as usize;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|_| unsupported())?;
    let mut converter = PcmConverter::new(rate)?;
    let (output, receiver) = async_mpsc::channel(64);
    thread::spawn(move || {
        let result = (|| -> Result<()> {
            let mut interleaved = Vec::<f32>::new();
            // A closed receiver means the import was cancelled.
            let send = |bytes: Vec<u8>| bytes.is_empty() || output.blocking_send(Ok(bytes)).is_ok();
            while let Some(packet) = format.next_packet().context("Audio could not be read")? {
                if packet.track_id != track_id {
                    continue;
                }
                let decoded = match decoder.decode(&packet) {
                    Ok(decoded) => decoded,
                    // Players skip a corrupt frame; so does an import.
                    Err(Error::DecodeError(_)) => continue,
                    Err(_) => bail!("Audio could not be read"),
                };
                let channels = decoded.spec().channels().count().max(1);
                decoded.copy_to_vec_interleaved(&mut interleaved);
                let mono: Vec<f32> = interleaved
                    .chunks_exact(channels)
                    .map(|frame| frame.iter().sum::<f32>() / channels as f32)
                    .collect();
                if !send(converter.push(&mono)?) {
                    return Ok(());
                }
            }
            send(converter.finish()?);
            Ok(())
        })();
        if let Err(e) = result {
            let _ = output.blocking_send(Err(e.to_string()));
        }
    });
    Ok(receiver)
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

/// Two soft notes: rising when recording starts, falling when it stops.
fn chime_samples(rate: f32, rising: bool) -> Vec<f32> {
    let (first, second) = if rising { (659.3, 880.) } else { (880., 659.3) };
    let note = |t: f32, start: f32, frequency: f32| {
        let t = t - start;
        if t < 0. {
            return 0.;
        }
        (t * 250.).min(1.) * (-t * 16.).exp() * (std::f32::consts::TAU * frequency * t).sin()
    };
    (0..(rate * 0.45) as usize)
        .map(|n| {
            let t = n as f32 / rate;
            0.14 * (note(t, 0., first) + note(t, 0.085, second))
        })
        .collect()
}

/// Plays the recording chime in the background. Missing or busy speakers
/// leave it silent rather than affecting the recording.
pub fn chime(rising: bool) {
    thread::spawn(move || {
        let _ = (|| -> Result<()> {
            let device = cpal::default_host()
                .default_output_device()
                .context("No speakers")?;
            let supported = device.default_output_config()?;
            let channels = supported.channels() as usize;
            let samples = chime_samples(supported.sample_rate().0 as f32, rising);
            let length =
                Duration::from_secs_f32(samples.len() as f32 / supported.sample_rate().0 as f32);
            macro_rules! build {
                ($ty:ty, $convert:expr) => {{
                    let mut position = 0;
                    let samples = samples.clone();
                    device.build_output_stream(
                        &supported.config(),
                        move |data: &mut [$ty], _| {
                            for frame in data.chunks_mut(channels) {
                                let sample: f32 = samples.get(position).copied().unwrap_or(0.);
                                position += 1;
                                frame.fill($convert(sample));
                            }
                        },
                        |_| {},
                        None,
                    )?
                }};
            }
            let stream = match supported.sample_format() {
                cpal::SampleFormat::F32 => build!(f32, |s: f32| s),
                cpal::SampleFormat::I16 => build!(i16, |s: f32| (s * 32767.) as i16),
                cpal::SampleFormat::U16 => build!(u16, |s: f32| (s * 32767. + 32768.) as u16),
                _ => bail!("Speaker format unsupported"),
            };
            stream.play()?;
            thread::sleep(length + Duration::from_millis(150));
            Ok(())
        })();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn level(frequency: f32) -> f32 {
        let rate = 48_000;
        let (mut rumble, mut hiss) = (
            Biquad::new(rate, 200., true),
            Biquad::new(rate, 4000., false),
        );
        let energy: f32 = (0..rate)
            .map(|n| {
                let x = (std::f32::consts::TAU * frequency * n as f32 / rate as f32).sin();
                hiss.process(rumble.process(x)).powi(2)
            })
            .skip(rate as usize / 10)
            .sum();
        (energy / (rate as f32 * 0.9)).sqrt()
    }

    #[test]
    fn level_meter_hears_the_voice_band_but_not_rumble_or_hiss() {
        let voice = level(1000.);
        assert!((voice - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.05);
        // Mains hum and high hiss are at least 18 dB down; low rumble, which
        // overlaps deep voices, about 12 dB.
        for frequency in [50., 60., 12_000.] {
            assert!(level(frequency) < voice / 8., "{frequency} Hz");
        }
        assert!(level(100.) < voice / 3.5);
    }

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

    #[tokio::test]
    async fn imports_decode_to_mono_pcm_at_the_stream_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        // One second of 44.1 kHz stereo PCM16.
        let frames = 44_100u32;
        let mut wav = Vec::new();
        wav.extend(b"RIFF");
        wav.extend((36 + frames * 4).to_le_bytes());
        wav.extend(b"WAVEfmt ");
        wav.extend(16u32.to_le_bytes());
        wav.extend(1u16.to_le_bytes()); // PCM
        wav.extend(2u16.to_le_bytes()); // channels
        wav.extend(44_100u32.to_le_bytes());
        wav.extend((44_100u32 * 4).to_le_bytes());
        wav.extend(4u16.to_le_bytes());
        wav.extend(16u16.to_le_bytes());
        wav.extend(b"data");
        wav.extend((frames * 4).to_le_bytes());
        for n in 0..frames {
            let s = ((n as f32 * 440. * std::f32::consts::TAU / 44_100.).sin() * 8000.) as i16;
            wav.extend_from_slice(&s.to_le_bytes());
            wav.extend_from_slice(&s.to_le_bytes());
        }
        std::fs::write(&path, wav).unwrap();
        let mut audio = decode(&path).unwrap();
        let mut bytes = 0;
        while let Some(packet) = audio.recv().await {
            bytes += packet.unwrap().len();
        }
        assert_eq!(bytes, SAMPLE_RATE * 2);

        std::fs::write(&path, b"not audio").unwrap();
        assert_eq!(
            decode(&path).err().unwrap().to_string(),
            "Audio format unsupported"
        );
    }

    #[test]
    fn chime_is_soft_and_ends_in_silence() {
        for rising in [true, false] {
            let samples = chime_samples(48_000., rising);
            assert!(samples.iter().all(|s| s.abs() < 0.3));
            assert!(samples.iter().any(|s| s.abs() > 0.08));
            assert!(samples[samples.len() - 100..]
                .iter()
                .all(|s| s.abs() < 0.01));
        }
    }
}
