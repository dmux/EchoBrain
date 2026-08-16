//! Microphone capture, via `cpal`.
//!
//! # Why there is a thread in here
//!
//! `cpal::Stream` is `!Send` on macOS — it is bound to the thread that created
//! it. [`echobrain_core::AudioCapture`] is `Send + Sync`, because the use cases
//! and Tauri's command handlers are. Those two facts cannot both hold in one
//! struct, so the stream lives on a worker thread that owns it for its whole
//! life, and this type talks to that thread over a channel.
//!
//! The alternative — making the port `!Send` — would push the constraint up
//! into the domain, where a detail of one macOS audio API has no business being.

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use echobrain_core::{AudioCapture, AudioMonitor, EchoError, Frame, Samples};

use crate::resample::to_whisper_input;

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no input device is available")]
    NoDevice,

    // cpal 0.18 collapsed its per-operation error types into one `Error` with
    // an `ErrorKind`, so the variants here carry the operation instead.
    #[error("the input device has no usable configuration")]
    NoConfig(#[source] cpal::Error),

    #[error("could not open the input stream — check the microphone permission")]
    Open(#[source] cpal::Error),

    #[error("could not start the input stream")]
    Play(#[source] cpal::Error),

    #[error("the capture thread stopped")]
    ThreadGone,

    #[error("nothing was captured")]
    Empty,

    #[error(transparent)]
    Resample(#[from] crate::resample::ResampleError),
}

impl From<CaptureError> for EchoError {
    fn from(error: CaptureError) -> Self {
        match error {
            // No sound is a normal outcome, not a device failure.
            CaptureError::Empty => EchoError::EmptyUtterance,
            other => EchoError::audio(other),
        }
    }
}

enum Command {
    Start(Sender<Result<(), CaptureError>>),
    Stop(Sender<Result<Samples, CaptureError>>),
    /// Begin delivering frames to `sink`, opening the device if it is closed.
    Monitor {
        sink: Sender<Frame>,
        reply: Sender<Result<(), CaptureError>>,
    },
    /// Open the device for monitoring without overwriting an existing sink.
    StartMonitoring {
        reply: Sender<Result<(), CaptureError>>,
    },
    /// Stop delivering frames. The device stays open if a recording is running.
    Unmonitor,
}

/// The default input device, recorded into memory between start and stop.
pub struct MicrophoneCapture {
    commands: Sender<Command>,
    /// Input level as `f32` bits, written from the audio callback. Atomic
    /// rather than a lock because the callback runs on a realtime thread where
    /// blocking on a mutex can produce an audible glitch.
    level: Arc<AtomicU32>,
    /// Whether frames are being delivered. Read by the UI to decide whether to
    /// show that the microphone is open.
    monitoring: Arc<AtomicBool>,
}

impl std::fmt::Debug for MicrophoneCapture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MicrophoneCapture")
            .field("level", &self.level())
            .finish_non_exhaustive()
    }
}

impl MicrophoneCapture {
    /// Spawns the worker thread. Does not open the device or trigger the macOS
    /// microphone permission prompt — that happens on the first
    /// [`AudioCapture::start`].
    pub fn new() -> Self {
        let (commands, inbox) = channel();
        let level = Arc::new(AtomicU32::new(0));

        let thread_level = Arc::clone(&level);
        let monitoring = Arc::new(AtomicBool::new(false));
        let thread_monitoring = Arc::clone(&monitoring);
        std::thread::Builder::new()
            .name("echobrain-audio".into())
            .spawn(move || worker(inbox, thread_level, thread_monitoring))
            // A thread that cannot be spawned means the process is in no state
            // to record anyway; the channel closing surfaces it as ThreadGone.
            .ok();

        Self {
            commands,
            level,
            monitoring,
        }
    }

    fn request<T>(
        &self,
        make: impl FnOnce(Sender<Result<T, CaptureError>>) -> Command,
    ) -> Result<T, CaptureError> {
        let (reply, answer) = channel();
        self.commands
            .send(make(reply))
            .map_err(|_| CaptureError::ThreadGone)?;
        answer.recv().map_err(|_| CaptureError::ThreadGone)?
    }
}

impl Default for MicrophoneCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl AudioCapture for MicrophoneCapture {
    async fn start(&self) -> Result<(), EchoError> {
        self.request(Command::Start)?;
        Ok(())
    }

    async fn stop(&self) -> Result<Samples, EchoError> {
        self.level.store(0, Ordering::Relaxed);
        Ok(self.request(Command::Stop)?)
    }

    fn level(&self) -> f32 {
        f32::from_bits(self.level.load(Ordering::Relaxed))
    }
}

#[async_trait]
impl AudioMonitor for MicrophoneCapture {
    async fn start_monitoring(&self) -> Result<(), EchoError> {
        // A caller that wants frames calls `frames()`; this only opens the
        // device. Split so the device can be brought up before a listener
        // exists — the conversation loop subscribes and then starts.
        // We don't want to overwrite an existing sink here if one is already active.
        // So we send a special command just to open the device if needed.
        self.request(|reply| Command::StartMonitoring { reply })?;
        Ok(())
    }

    fn frames(&self) -> Receiver<Frame> {
        let (sink, receiver) = channel();
        // Not `request`: the caller wants the receiver now, and a device that
        // fails to open surfaces as a channel that never yields rather than as
        // an error here. `start_monitoring` is where opening is reported.
        let (reply, answer) = channel();
        if self.commands.send(Command::Monitor { sink, reply }).is_ok() {
            let _ = answer.recv();
        }
        receiver
    }

    fn stop_monitoring(&self) {
        let _ = self.commands.send(Command::Unmonitor);
    }

    fn is_monitoring(&self) -> bool {
        self.monitoring.load(Ordering::Relaxed)
    }
}

/// The buffer a recording accumulates into, as both sides hold it.
type RecordingBuffer = Arc<Mutex<Vec<f32>>>;

/// The buffer the *callback* sees, which the worker swaps between commands.
///
/// Two layers of `Mutex` because they protect different things: the outer one
/// guards the swap, the inner one guards the samples. Collapsing them would
/// mean the worker had to hold the sample lock to change which buffer is live,
/// which is the lock the realtime callback needs.
type LiveRecording = Arc<Mutex<Option<RecordingBuffer>>>;

/// The open device, and whatever is currently consuming it.
///
/// One stream serves both jobs. Opening a second would mean two callbacks on
/// one device, two permission prompts' worth of confusion, and — on macOS —
/// often just an error.
struct Open {
    _stream: cpal::Stream,
    /// `Some` while a batch recording is accumulating. Owned by the worker.
    recording: Option<RecordingBuffer>,
    /// The same buffer, as the callback sees it. Separate from `recording`
    /// because the worker swaps it between commands while the callback is
    /// running, and the callback may only ever `try_lock`.
    live_recording: LiveRecording,
    /// `Some` while frames are being delivered.
    sink: Arc<Mutex<Option<Sender<Frame>>>>,
    sample_rate: u32,
    channels: u16,
}

fn worker(inbox: Receiver<Command>, level: Arc<AtomicU32>, monitoring: Arc<AtomicBool>) {
    let mut open_device: Option<Open> = None;

    while let Ok(command) = inbox.recv() {
        match command {
            Command::Start(reply) => {
                let result = match open_device.as_mut() {
                    // Already open for monitoring: attach a recording buffer to
                    // the same stream instead of opening a second one.
                    Some(active) => {
                        if active.recording.is_none() {
                            active.recording = Some(Arc::new(Mutex::new(Vec::new())));
                        }
                        Ok(())
                    }
                    None => match open(Arc::clone(&level)) {
                        Ok(mut active) => {
                            active.recording = Some(Arc::new(Mutex::new(Vec::new())));
                            open_device = Some(active);
                            Ok(())
                        }
                        Err(error) => Err(error),
                    },
                };

                // The callback reads the recording buffer through the same
                // `Open`, so it has to be published before the reply unblocks
                // the caller — otherwise the first few milliseconds of speech
                // land nowhere.
                if let Some(active) = open_device.as_ref() {
                    active.publish_recording();
                }
                let _ = reply.send(result);
            }

            Command::Stop(reply) => {
                let result = match open_device.as_mut() {
                    None => Err(CaptureError::Empty),
                    Some(active) => {
                        let taken = active.recording.take();
                        active.publish_recording();

                        // Keep the device open if frames are still wanted;
                        // closing and reopening it mid-conversation drops
                        // audio and, on macOS, sometimes fails outright.
                        let still_needed = active.has_sink();

                        let captured = taken.map(|buffer| match buffer.lock() {
                            Ok(guard) => guard.clone(),
                            Err(poisoned) => poisoned.into_inner().clone(),
                        });
                        let rate = active.sample_rate;
                        let channels = active.channels;

                        if !still_needed {
                            open_device = None;
                            level.store(0, Ordering::Relaxed);
                        }

                        match captured {
                            Some(samples) if !samples.is_empty() => {
                                to_whisper_input(&samples, rate, channels)
                                    .map_err(CaptureError::from)
                            }
                            _ => Err(CaptureError::Empty),
                        }
                    }
                };
                let _ = reply.send(result);
            }

            Command::Monitor { sink, reply } => {
                let result = match open_device.as_ref() {
                    Some(active) => {
                        active.set_sink(Some(sink));
                        Ok(())
                    }
                    None => match open(Arc::clone(&level)) {
                        Ok(active) => {
                            active.set_sink(Some(sink));
                            open_device = Some(active);
                            Ok(())
                        }
                        Err(error) => Err(error),
                    },
                };
                monitoring.store(result.is_ok(), Ordering::Relaxed);
                let _ = reply.send(result);
            }

            Command::StartMonitoring { reply } => {
                let result = match open_device.as_ref() {
                    Some(_active) => Ok(()),
                    None => match open(Arc::clone(&level)) {
                        Ok(active) => {
                            open_device = Some(active);
                            Ok(())
                        }
                        Err(error) => Err(error),
                    },
                };
                monitoring.store(result.is_ok(), Ordering::Relaxed);
                let _ = reply.send(result);
            }

            Command::Unmonitor => {
                monitoring.store(false, Ordering::Relaxed);
                if let Some(active) = open_device.as_ref() {
                    active.set_sink(None);
                    // Nothing left to serve — close the device so the
                    // microphone light goes out.
                    if active.recording.is_none() {
                        open_device = None;
                        level.store(0, Ordering::Relaxed);
                    }
                }
            }
        }
    }
}

impl Open {
    fn set_sink(&self, sink: Option<Sender<Frame>>) {
        match self.sink.lock() {
            Ok(mut guard) => *guard = sink,
            Err(poisoned) => *poisoned.into_inner() = sink,
        }
    }

    fn has_sink(&self) -> bool {
        match self.sink.lock() {
            Ok(guard) => guard.is_some(),
            Err(poisoned) => poisoned.into_inner().is_some(),
        }
    }

    /// Hand the callback the current recording buffer, or take it away.
    fn publish_recording(&self) {
        let next = self.recording.clone();
        match self.live_recording.lock() {
            Ok(mut guard) => *guard = next,
            Err(poisoned) => *poisoned.into_inner() = next,
        }
    }
}

fn open(level: Arc<AtomicU32>) -> Result<Open, CaptureError> {
    let host = cpal::default_host();
    let device = host.default_input_device().ok_or(CaptureError::NoDevice)?;
    let config = device
        .default_input_config()
        .map_err(CaptureError::NoConfig)?;

    let sample_rate = config.sample_rate();
    let channels = config.channels();

    let sink: Arc<Mutex<Option<Sender<Frame>>>> = Arc::new(Mutex::new(None));
    let live_recording: LiveRecording = Arc::new(Mutex::new(None));

    let callback_sink = Arc::clone(&sink);
    let callback_recording = Arc::clone(&live_recording);

    let stream = device
        .build_input_stream(
            config.config(),
            move |data: &[f32], _: &cpal::InputCallbackInfo| {
                if data.is_empty() {
                    return;
                }
                // RMS, not peak: peak jumps on a single click and makes the
                // waveform twitch; RMS tracks how loud the speech actually is.
                let rms = (data.iter().map(|s| s * s).sum::<f32>() / data.len() as f32).sqrt();
                level.store(rms.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);

                if let Ok(guard) = callback_recording.try_lock()
                    && let Some(buffer) = guard.as_ref()
                    && let Ok(mut samples) = buffer.try_lock()
                {
                    samples.extend_from_slice(data);
                }

                // Everything below runs on a realtime thread. `try_lock` and a
                // non-blocking send are not an optimisation here: waiting for
                // either would stall the audio driver and produce an audible
                // glitch, and a monitor that fell behind is not worth that.
                let Ok(guard) = callback_sink.try_lock() else {
                    return;
                };
                let Some(sender) = guard.as_ref() else {
                    return;
                };

                // Mono. A frame is for deciding whether someone is speaking, and
                // averaging is both cheaper and less directional than picking a
                // channel.
                let mono: Vec<f32> = if channels <= 1 {
                    data.to_vec()
                } else {
                    data.chunks(channels as usize)
                        .map(|frame| frame.iter().sum::<f32>() / frame.len() as f32)
                        .collect()
                };

                // A closed channel means the listener went away; the sink is
                // cleared on the next `Unmonitor` rather than from here, since
                // this thread must not take a blocking lock.
                let _ = sender.send(Frame {
                    samples: mono,
                    sample_rate,
                });
            },
            |error| tracing::warn!(%error, "input stream error"),
            None,
        )
        .map_err(CaptureError::Open)?;

    stream.play().map_err(CaptureError::Play)?;

    Ok(Open {
        _stream: stream,
        recording: None,
        sink,
        live_recording,
        sample_rate,
        channels,
    })
}
