//! System audio capture on macOS.
//!
//! This is the "what the machine is playing" side: the other people on a call,
//! a recorded talk, whatever is coming out of the speakers. It is a capture
//! path, not a use case, and it is what makes jay useful on an incident bridge
//! or in a pairing session rather than only when you talk to yourself.
//!
//! Implemented as a CoreAudio process tap (`AudioHardwareCreateProcessTap`,
//! macOS 14.4+) behind an Objective-C shim in `macos/system_tap.m`.
//! ScreenCaptureKit would also work and is more widely copied, but it drags in
//! the screen-recording permission even when only audio is wanted, and asking
//! for the camera roll of someone's desktop to transcribe a meeting is rude.
//!
//! macOS will ask the user for permission the first time. That is correct.

use std::ffi::c_void;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::Sender;
use ringbuf::HeapRb;
use ringbuf::traits::{Consumer, Producer, Split};

use crate::resample::Downsampler;
use crate::{AudioError, Channel, Frame, Result};

const RING_CAPACITY: usize = 96_000;
const IDLE_POLL: Duration = Duration::from_millis(5);

/// Codes the shim returns for problems that are not an `OSStatus`.
const TAP_UNSUPPORTED_OS: i32 = -1;
const TAP_ALLOC_FAILED: i32 = -2;
const TAP_NO_OUTPUT_DEVICE: i32 = -3;

type TapCallback = extern "C" fn(*mut c_void, *const f32, u32);

unsafe extern "C" {
    fn jay_system_tap_start(
        callback: TapCallback,
        ctx: *mut c_void,
        out_handle: *mut *mut c_void,
        out_sample_rate: *mut f64,
    ) -> i32;
    fn jay_system_tap_stop(handle: *mut c_void);
    fn jay_default_output_name(buffer: *mut std::os::raw::c_char, len: usize) -> i32;
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

/// Name of the output device the tap will attach itself to.
///
/// Read once, at start, exactly as the tap does — which is the point. The
/// aggregate device is built around whichever output device is default at that
/// instant and never revisits the question, so a session where the call is in
/// headphones and this says "MacBook Pro Speakers" is a session that will hear
/// nothing on the `them` channel and give no other sign of it.
pub fn default_output_name() -> Option<String> {
    let mut buffer = [0i8; 256];
    // SAFETY: the shim writes at most `len` bytes including the terminator into
    // the buffer we own, and reports failure rather than writing on error.
    let ok = unsafe { jay_default_output_name(buffer.as_mut_ptr(), buffer.len()) } == 0;
    if !ok {
        return None;
    }
    // SAFETY: on success the shim has written a NUL-terminated UTF-8 string.
    let name = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) }
        .to_string_lossy()
        .into_owned();
    (!name.is_empty()).then_some(name)
}

/// What the CoreAudio callback writes into. Boxed and handed to the shim as an
/// opaque context pointer, and reclaimed when the tap stops.
struct TapContext {
    producer: ringbuf::HeapProd<f32>,
    dropped: Arc<AtomicU64>,
}

/// Called from a CoreAudio real-time thread. Does nothing that allocates,
/// locks or blocks, because all three would be heard rather than merely
/// measured.
extern "C" fn on_audio(ctx: *mut c_void, samples: *const f32, frames: u32) {
    if ctx.is_null() || samples.is_null() || frames == 0 {
        return;
    }
    // SAFETY: `ctx` is the pointer handed to the shim in `start`, which keeps
    // the box alive until after `jay_system_tap_stop` has returned.
    let context = unsafe { &mut *(ctx as *mut TapContext) };
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

/// A running system-audio tap. Dropping it stops the tap and joins the worker.
pub struct SystemCapture {
    handle: *mut c_void,
    context: *mut TapContext,
    stop: Arc<AtomicBool>,
    dropped: Arc<AtomicU64>,
    worker: Option<JoinHandle<()>>,
    sample_rate: u32,
}

// SAFETY: the raw pointers are owned by this struct and only touched in `Drop`
// and by the CoreAudio thread, which is stopped before either is freed.
unsafe impl Send for SystemCapture {}

impl SystemCapture {
    /// Sample rate the tap reported, before resampling to 16 kHz.
    pub fn device_sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn dropped_samples(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

impl Drop for SystemCapture {
    fn drop(&mut self) {
        // Order matters: stop the tap first, so the callback cannot be running
        // when the context it writes into is freed.
        if !self.handle.is_null() {
            unsafe { jay_system_tap_stop(self.handle) };
            self.handle = std::ptr::null_mut();
        }
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if !self.context.is_null() {
            // SAFETY: the tap is stopped, so nothing else refers to this.
            drop(unsafe { Box::from_raw(self.context) });
            self.context = std::ptr::null_mut();
        }
    }
}

/// Start capturing system output audio.
///
/// Frames arrive on `tx` tagged [`Channel::System`]. The first call prompts the
/// user for permission, and will fail until they grant it.
pub fn start(tx: Sender<Frame>) -> Result<SystemCapture> {
    let ring = HeapRb::<f32>::new(RING_CAPACITY);
    let (producer, mut consumer) = ring.split();

    let dropped = Arc::new(AtomicU64::new(0));
    let context = Box::into_raw(Box::new(TapContext {
        producer,
        dropped: Arc::clone(&dropped),
    }));

    let mut handle: *mut c_void = std::ptr::null_mut();
    let mut sample_rate: f64 = 0.0;

    // SAFETY: `context` stays alive until `SystemCapture` is dropped, which
    // stops the tap before freeing it.
    let status = unsafe {
        jay_system_tap_start(
            on_audio,
            context as *mut c_void,
            &mut handle,
            &mut sample_rate,
        )
    };

    if status != 0 || handle.is_null() {
        // SAFETY: the tap never started, so nothing else holds this.
        drop(unsafe { Box::from_raw(context) });
        return Err(tap_error(status));
    }

    let device_rate = sample_rate.round() as u32;
    tracing::info!(rate = device_rate, "system audio tap running");

    let stop = Arc::new(AtomicBool::new(false));
    let worker = {
        let stop = Arc::clone(&stop);
        let mut resampler = Downsampler::new(device_rate)?;
        thread::Builder::new()
            .name("jay-system-worker".into())
            .spawn(move || {
                let mut raw = vec![0.0f32; 4096];
                while !stop.load(Ordering::Relaxed) {
                    let read = Consumer::pop_slice(&mut consumer, &mut raw);
                    if read == 0 {
                        thread::sleep(IDLE_POLL);
                        continue;
                    }

                    // Already mono: the shim mixes down before handing over.
                    if let Err(e) = resampler.push(&raw[..read]) {
                        tracing::error!(%e, "resampling system audio failed");
                        continue;
                    }

                    let captured_at = Instant::now();
                    while let Some(samples) = resampler.take_frame() {
                        let frame = Frame {
                            channel: Channel::System,
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

    Ok(SystemCapture {
        handle,
        context,
        stop,
        dropped,
        worker: Some(worker),
        sample_rate: device_rate,
    })
}

fn tap_error(status: i32) -> AudioError {
    match status {
        TAP_UNSUPPORTED_OS => AudioError::SystemTap(
            "process taps need macOS 14.4 or newer".to_string(),
        ),
        TAP_ALLOC_FAILED => AudioError::SystemTap("could not allocate the tap".to_string()),
        TAP_NO_OUTPUT_DEVICE => AudioError::SystemTap(
            "no default output device, so there is nothing to tap".to_string(),
        ),
        // Everything else is an OSStatus. The four-character-code spelling is
        // what Apple's documentation uses, so print both.
        other => AudioError::SystemTap(format!(
            "CoreAudio returned {other} ({}). If this is a permission error, \
             grant audio recording in System Settings > Privacy & Security",
            four_cc(other)
        )),
    }
}

/// Render an OSStatus as the four-character code Apple documents it by.
fn four_cc(status: i32) -> String {
    let bytes = (status as u32).to_be_bytes();
    if bytes.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
        format!("'{}'", String::from_utf8_lossy(&bytes))
    } else {
        format!("0x{status:08x}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_os_status_as_four_char_codes() {
        // 'nope' is what CoreAudio returns for an unsupported operation.
        let nope = i32::from_be_bytes(*b"nope");
        assert_eq!(four_cc(nope), "'nope'");
        assert_eq!(four_cc(-1), "0xffffffff");
    }
}
