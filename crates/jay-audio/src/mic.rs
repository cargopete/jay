//! Microphone capture via cpal.
//!
//! The device callback does as little as possible: convert to `f32` and push
//! into a lock-free ring. Downmixing, resampling and framing all happen on a
//! worker thread, because doing them in the callback risks an xrun and an xrun
//! sounds exactly like a word the transcript quietly loses.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::Sender;
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::resample::{Downsampler, downmix};
use crate::{AudioError, Channel, Frame, Result};

/// Ring capacity in samples: roughly one second of 48 kHz stereo. Generous,
/// since the memory is trivial and an underrun here costs speech.
const RING_CAPACITY: usize = 96_000;

/// How long the worker sleeps when the ring is empty. Well under the 20 ms
/// frame period, so it adds no meaningful latency.
const IDLE_POLL: Duration = Duration::from_millis(5);

/// A running capture. Dropping it stops the stream and joins the threads.
pub struct Capture {
    stop: Arc<AtomicBool>,
    dropped_samples: Arc<AtomicU64>,
    threads: Vec<JoinHandle<()>>,
}

impl Capture {
    /// Samples the device produced that the ring had no room for.
    ///
    /// Should be zero. If it is not, the worker is not keeping up and the
    /// transcript is missing audio, which is worth surfacing rather than
    /// discovering later as mysteriously dropped words.
    pub fn dropped_samples(&self) -> u64 {
        self.dropped_samples.load(Ordering::Relaxed)
    }

    pub fn stop(self) {}
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.threads.drain(..) {
            let _ = handle.join();
        }
    }
}

/// The device-callback side of the ring.
///
/// Kept as a struct rather than a closure so each sample format can reuse the
/// same overflow accounting without the borrow checker taking a view on it.
struct RingSink<P> {
    producer: P,
    dropped: Arc<AtomicU64>,
    scratch: Vec<f32>,
}

impl<P: Producer<Item = f32>> RingSink<P> {
    fn push(&mut self, samples: &[f32]) {
        let written = self.producer.push_slice(samples);
        if written < samples.len() {
            self.dropped
                .fetch_add((samples.len() - written) as u64, Ordering::Relaxed);
        }
    }

    fn push_converted<T: Copy>(&mut self, samples: &[T], to_f32: impl Fn(T) -> f32) {
        self.scratch.clear();
        self.scratch.extend(samples.iter().copied().map(to_f32));
        let written = self.producer.push_slice(&self.scratch);
        if written < self.scratch.len() {
            self.dropped
                .fetch_add((self.scratch.len() - written) as u64, Ordering::Relaxed);
        }
    }
}

/// Hand a frame downstream, giving up if the capture has been told to stop.
///
/// A plain `send` blocks forever once the channel is full, and the channel
/// fills the moment the pipeline stops reading it — which is exactly what
/// happens at the end of a session, while the decoder is still working through
/// its backlog. The capture thread then parks inside `send`, never sees `stop`,
/// and `Drop` joins it and waits for a thread that cannot finish. A 80-second
/// session hung on that with its transcript complete and its panel still open.
///
/// Returns `false` when the caller should stop: either the receiver is gone or
/// the capture has been asked to end.
fn send_unless_stopped(
    tx: &Sender<Frame>,
    frame: Frame,
    stop: &std::sync::atomic::AtomicBool,
) -> bool {
    let mut pending = frame;
    loop {
        match tx.send_timeout(pending, IDLE_POLL) {
            Ok(()) => return true,
            Err(crossbeam_channel::SendTimeoutError::Timeout(returned)) => {
                if stop.load(Ordering::Relaxed) {
                    return false;
                }
                pending = returned;
            }
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => return false,
        }
    }
}

/// List input device names, for `--list-devices` and for config validation.
pub fn input_devices() -> Result<Vec<String>> {
    let host = cpal::default_host();
    let devices = host
        .input_devices()
        .map_err(|e| AudioError::Cpal(anyhow::anyhow!(e)))?;
    Ok(devices
        .filter_map(|d| d.description().ok())
        .map(|d| d.name().to_string())
        .collect())
}

/// Start capturing from the named input device, or the system default.
///
/// Frames arrive on `tx` tagged [`Channel::Mic`].
pub fn start(device_name: Option<&str>, tx: Sender<Frame>) -> Result<Capture> {
    let host = cpal::default_host();

    let device = match device_name {
        Some(wanted) => host
            .input_devices()
            .map_err(|e| AudioError::Cpal(anyhow::anyhow!(e)))?
            .find(|d| d.description().is_ok_and(|desc| desc.name() == wanted))
            .ok_or(AudioError::NoDevice("matching input"))?,
        None => host
            .default_input_device()
            .ok_or(AudioError::NoDevice("default input"))?,
    };

    let supported = device
        .default_input_config()
        .map_err(|e| AudioError::UnsupportedConfig(e.to_string()))?;

    let device_label = device
        .description()
        .map(|d| d.name().to_string())
        .unwrap_or_else(|_| "<unnamed>".to_string());

    let sample_format = supported.sample_format();
    let config: cpal::StreamConfig = supported.into();
    let channels = config.channels as usize;
    let device_rate = config.sample_rate;

    tracing::info!(
        device = %device_label,
        rate = device_rate,
        channels,
        ?sample_format,
        "opening microphone"
    );

    let ring = HeapRb::<f32>::new(RING_CAPACITY);
    let (producer, mut consumer) = ring.split();

    let stop = Arc::new(AtomicBool::new(false));
    let dropped = Arc::new(AtomicU64::new(0));

    // Audio thread: owns the cpal stream, which is `!Send` on macOS and so
    // cannot simply be stashed in the struct.
    let audio_thread = {
        let stop = Arc::clone(&stop);
        let dropped = Arc::clone(&dropped);
        thread::Builder::new()
            .name("jay-mic-device".into())
            .spawn(move || {
                let err_fn = |err| tracing::error!(%err, "microphone stream error");

                let mut sink = RingSink {
                    producer,
                    dropped,
                    scratch: Vec::with_capacity(8192),
                };

                let stream = match sample_format {
                    cpal::SampleFormat::F32 => device.build_input_stream(
                        config,
                        move |data: &[f32], _: &_| sink.push(data),
                        err_fn,
                        None,
                    ),
                    cpal::SampleFormat::I16 => device.build_input_stream(
                        config,
                        move |data: &[i16], _: &_| sink.push_converted(data, |s| s as f32 / 32768.0),
                        err_fn,
                        None,
                    ),
                    cpal::SampleFormat::U16 => device.build_input_stream(
                        config,
                        move |data: &[u16], _: &_| {
                            sink.push_converted(data, |s| (s as f32 - 32768.0) / 32768.0)
                        },
                        err_fn,
                        None,
                    ),
                    other => {
                        tracing::error!(?other, "unsupported sample format");
                        return;
                    }
                };

                let stream = match stream {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!(%e, "could not build microphone stream");
                        return;
                    }
                };
                if let Err(e) = stream.play() {
                    tracing::error!(%e, "could not start microphone stream");
                    return;
                }

                while !stop.load(Ordering::Relaxed) {
                    thread::sleep(IDLE_POLL);
                }
            })
            .map_err(|e| AudioError::Cpal(anyhow::anyhow!(e)))?
    };

    // Worker thread: ring -> mono -> 16 kHz -> fixed frames -> channel.
    let worker_thread = {
        let stop = Arc::clone(&stop);
        let mut resampler = Downsampler::new(device_rate)?;
        thread::Builder::new()
            .name("jay-mic-worker".into())
            .spawn(move || {
                let mut raw = vec![0.0f32; 4096];
                let mut mono = Vec::with_capacity(4096);
                // A read can land mid-interleaved-frame. The tail carries over
                // rather than being dropped, or the channel order would rotate
                // and the downmix would quietly start averaging the wrong pairs.
                let mut carry: Vec<f32> = Vec::with_capacity(channels);

                while !stop.load(Ordering::Relaxed) {
                    let read = Consumer::pop_slice(&mut consumer, &mut raw);
                    if read == 0 {
                        thread::sleep(IDLE_POLL);
                        continue;
                    }

                    carry.extend_from_slice(&raw[..read]);
                    let usable = carry.len() - (carry.len() % channels);
                    downmix(&carry[..usable], channels, &mut mono);
                    carry.drain(..usable);

                    if let Err(e) = resampler.push(&mono) {
                        tracing::error!(%e, "resampling microphone audio failed");
                        continue;
                    }

                    let captured_at = Instant::now();
                    while let Some(samples) = resampler.take_frame() {
                        let frame = Frame {
                            channel: Channel::Mic,
                            samples,
                            captured_at,
                        };
                        if !send_unless_stopped(&tx, frame, &stop) {
                            return;
                        }
                    }
                }
            })
            .map_err(|e| AudioError::Cpal(anyhow::anyhow!(e)))?
    };

    Ok(Capture {
        stop,
        dropped_samples: dropped,
        threads: vec![audio_thread, worker_thread],
    })
}
