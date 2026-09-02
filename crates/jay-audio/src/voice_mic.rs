//! Microphone capture with the room cancelled out.
//!
//! [`mic`](crate::mic) opens the device through cpal and hears everything,
//! including whatever the speakers are playing. On a call taken without
//! headphones that is not a cosmetic problem. Measured on a real 19-minute
//! meeting: the microphone sat at 0.0408 RMS against a speech level of about
//! 0.02, never fell silent, and so the segmenter never reached its exit
//! condition. Nineteen of the fifty-five microphone utterances ran to the
//! 25-second cap in [`vad`](crate::vad) and were cut mid-sentence, each one
//! holding words from both people. Twenty-seven began on a lowercase word.
//!
//! The text-side echo guard cannot repair that. It compares a microphone line
//! against a system line and expects them to be copies of one sentence; once
//! the segmenters disagree about where sentences are, there is nothing left to
//! match. The two channels' start times differed by nine to sixteen seconds
//! against a two-second window.
//!
//! So the fix belongs before the VAD rather than after the transcript. This
//! path opens the microphone through `kAudioUnitSubType_VoiceProcessingIO`,
//! the same acoustic echo canceller every call application on this platform
//! uses, and asks it for 16 kHz mono directly, which is what whisper wants and
//! removes the resampler from this path entirely.
//!
//! Whether it helps is an empirical question and this module does not assume
//! an answer: [`start`] takes a `bypass` flag that runs the identical unit with
//! cancellation switched off, so the two can be measured against each other.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::{AudioError, Channel, FRAME_SAMPLES, Frame, Result};

/// Roughly one second at 16 kHz mono. Generous; the memory is nothing and an
/// overrun here costs speech.
const RING_CAPACITY: usize = 16_000;

/// How long the worker sleeps when the ring is empty. Well under the 32 ms
/// frame period implied by [`FRAME_SAMPLES`], so it adds no real latency.
const IDLE_POLL: Duration = Duration::from_millis(5);

/// Codes the shim returns for problems that are not an `OSStatus`.
const VOICE_MIC_ALLOC_FAILED: i32 = -2;
const VOICE_MIC_NO_COMPONENT: i32 = -4;

type VoiceMicCallback = extern "C" fn(*mut c_void, *const f32, u32);

unsafe extern "C" {
    fn jay_voice_mic_start(
        callback: VoiceMicCallback,
        ctx: *mut c_void,
        bypass: i32,
        out_handle: *mut *mut c_void,
        out_agc_status: *mut i32,
    ) -> i32;
    fn jay_voice_mic_stop(handle: *mut c_void);
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

/// What the CoreAudio callback writes into. Boxed and handed to the shim as an
/// opaque context pointer, and reclaimed when the unit stops.
struct MicContext {
    producer: ringbuf::HeapProd<f32>,
    dropped: Arc<AtomicU64>,
}

/// Called from a CoreAudio real-time thread. Allocates nothing, locks nothing
/// and blocks on nothing, because all three would be audible rather than
/// merely measurable.
extern "C" fn on_audio(ctx: *mut c_void, samples: *const f32, frames: u32) {
    if ctx.is_null() || samples.is_null() || frames == 0 {
        return;
    }
    // SAFETY: `ctx` is the pointer handed to the shim in `start`, which keeps
    // the box alive until after `jay_voice_mic_stop` has returned.
    let context = unsafe { &mut *(ctx as *mut MicContext) };
    // SAFETY: the shim guarantees `frames` valid floats at `samples` for the
    // duration of this call.
    let slice = unsafe { std::slice::from_raw_parts(samples, frames as usize) };

    let written = context.producer.push_slice(slice);
    if written < slice.len() {
        context
            .dropped
            .fetch_add((slice.len() - written) as u64, Ordering::Relaxed);
    }
}

/// A running echo-cancelled microphone. Dropping it stops the unit and joins
/// the worker.
pub struct VoiceCapture {
    handle: *mut c_void,
    context: *mut MicContext,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
}

// SAFETY: the raw pointers are owned by this struct and only touched in `Drop`
// and by the CoreAudio thread, which is stopped before either is freed.
unsafe impl Send for VoiceCapture {}

impl VoiceCapture {
    /// Samples the unit produced that the ring had no room for. Should be zero.
    pub fn dropped_samples(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for VoiceCapture {
    fn drop(&mut self) {
        // Order matters: stop the unit first, so the callback cannot be running
        // when the context it writes into is freed.
        if !self.handle.is_null() {
            unsafe { jay_voice_mic_stop(self.handle) };
            self.handle = std::ptr::null_mut();
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if !self.context.is_null() {
            // SAFETY: the unit is stopped, so nothing else refers to this.
            drop(unsafe { Box::from_raw(self.context) });
            self.context = std::ptr::null_mut();
        }
    }
}

/// Start the echo-cancelling microphone.
///
/// Frames arrive on `tx` tagged [`Channel::Mic`], already at 16 kHz mono, so
/// this path is interchangeable with [`mic::start`](crate::mic::start) from the
/// caller's point of view.
///
/// `bypass` runs the same unit with cancellation disabled. It exists so the
/// effect can be measured rather than asserted, and nothing in normal operation
/// should pass `true`.
pub fn start(tx: Sender<Frame>, bypass: bool) -> Result<VoiceCapture> {
    let ring = HeapRb::<f32>::new(RING_CAPACITY);
    let (producer, mut consumer) = ring.split();

    let dropped = Arc::new(AtomicU64::new(0));
    let context = Box::into_raw(Box::new(MicContext {
        producer,
        dropped: Arc::clone(&dropped),
    }));

    let mut handle: *mut c_void = std::ptr::null_mut();
    let mut agc_status: i32 = 0;

    // SAFETY: `context` stays alive until `VoiceCapture` is dropped, which
    // stops the unit before freeing it.
    let status = unsafe {
        jay_voice_mic_start(
            on_audio,
            context as *mut c_void,
            i32::from(bypass),
            &mut handle,
            &mut agc_status,
        )
    };

    if status != 0 || handle.is_null() {
        // SAFETY: the unit never started, so nothing else holds this.
        drop(unsafe { Box::from_raw(context) });
        return Err(match status {
            VOICE_MIC_NO_COMPONENT => {
                AudioError::UnsupportedConfig("no voice-processing audio unit".to_string())
            }
            VOICE_MIC_ALLOC_FAILED => {
                AudioError::UnsupportedConfig("voice-processing allocation failed".to_string())
            }
            other => AudioError::UnsupportedConfig(format!(
                "voice-processing unit failed to start (OSStatus {other})"
            )),
        });
    }

    // Loud, because a gain control left on invalidates every level threshold
    // downstream and does it silently.
    if agc_status != 0 {
        tracing::warn!(
            status = agc_status,
            "could not disable automatic gain control: levels will not be comparable"
        );
    }
    tracing::info!(
        bypass,
        agc_disabled = agc_status == 0,
        rate = crate::SAMPLE_RATE,
        "opened echo-cancelled microphone"
    );

    let stop = Arc::new(AtomicBool::new(false));

    // Worker: ring -> fixed frames -> channel. No downmix and no resampler,
    // because the unit was asked for exactly 16 kHz mono.
    let worker = {
        let stop = Arc::clone(&stop);
        thread::Builder::new()
            .name("jay-voice-mic".into())
            .spawn(move || {
                let mut pending: Vec<f32> = Vec::with_capacity(FRAME_SAMPLES * 4);
                let mut raw = vec![0.0f32; 4096];

                while !stop.load(Ordering::Relaxed) {
                    let read = Consumer::pop_slice(&mut consumer, &mut raw);
                    if read == 0 {
                        thread::sleep(IDLE_POLL);
                        continue;
                    }
                    pending.extend_from_slice(&raw[..read]);

                    let captured_at = Instant::now();
                    while pending.len() >= FRAME_SAMPLES {
                        let samples: Vec<f32> = pending.drain(..FRAME_SAMPLES).collect();
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

    Ok(VoiceCapture {
        handle,
        context,
        stop,
        dropped,
        worker: Some(worker),
    })
}
