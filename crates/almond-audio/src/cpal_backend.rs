//! Desktop capture via `cpal`:
//!
//! | channel  | Windows                                   | Linux (Fedora)                                    |
//! |----------|-------------------------------------------|---------------------------------------------------|
//! | `Mic`    | WASAPI default capture endpoint           | PulseAudio host (pipewire-pulse) default source; ALSA if no Pulse server |
//! | `System` | WASAPI **loopback** on the default render endpoint | `<default sink>.monitor` source via the PulseAudio host |
//!
//! cpal's PulseAudio host is a pure-Rust protocol client, so no libpulse/libpipewire/bindgen is
//! needed; on Fedora it talks to `pipewire-pulse`, which exposes sink monitors like PulseAudio.
//!
//! Each opened channel runs on its own thread that owns the (non-`Send` on some hosts) cpal
//! stream and supervises it: on a stream error (device unplugged, endpoint invalidated) or, when
//! following the default device, on a default-device change (e.g. headset plugged in), the stream
//! is torn down and re-opened. The gap is zero-filled by the alignment stage, so `t0_ms` stays
//! on the shared clock.

use crate::clock::CaptureClock;
use crate::pipeline::{AlignConfig, PipelineStream, RawSender, channel_pair};
use almond_types::audio::{AudioBackend, AudioDeviceInfo, AudioStream, CaptureConfig};
use almond_types::meeting::AudioChannel;
use almond_types::{Error, Result};
use async_trait::async_trait;
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, SizedSample};
use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// cpal-based [`AudioBackend`]. All streams opened from one backend share a [`CaptureClock`]
/// (started on first `open`, or explicitly with [`CpalBackend::reset_clock`]).
#[derive(Default)]
pub struct CpalBackend {
    clock: Mutex<Option<CaptureClock>>,
}

impl CpalBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_clock(clock: CaptureClock) -> Self {
        Self {
            clock: Mutex::new(Some(clock)),
        }
    }

    /// The session clock (started now if not yet started).
    pub fn clock(&self) -> CaptureClock {
        *self
            .clock
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(CaptureClock::start)
    }

    /// Start a new session clock; streams opened afterwards use it.
    pub fn reset_clock(&self) -> CaptureClock {
        let c = CaptureClock::start();
        *self.clock.lock().unwrap_or_else(|p| p.into_inner()) = Some(c);
        c
    }

    /// Like [`AudioBackend::open`] but returns the concrete stream (gives access to
    /// [`PipelineStream::stats`]). Blocks until the device is running (or fails).
    pub fn open_stream(
        &self,
        channel: AudioChannel,
        cfg: &CaptureConfig,
    ) -> Result<PipelineStream> {
        open_blocking(self.clock(), channel, cfg)
    }
}

#[async_trait]
impl AudioBackend for CpalBackend {
    fn name(&self) -> &str {
        "cpal"
    }

    async fn list_devices(&self) -> Result<Vec<AudioDeviceInfo>> {
        tokio::task::spawn_blocking(list_devices_blocking)
            .await
            .map_err(Error::other)?
    }

    async fn open(
        &self,
        channel: AudioChannel,
        cfg: CaptureConfig,
    ) -> Result<Box<dyn AudioStream>> {
        let clock = self.clock();
        let s = tokio::task::spawn_blocking(move || open_blocking(clock, channel, &cfg))
            .await
            .map_err(Error::other)??;
        Ok(Box::new(s))
    }
}

fn cpal_err(e: cpal::Error) -> Error {
    use cpal::ErrorKind as K;
    match e.kind() {
        K::DeviceNotAvailable => Error::NotFound(e.to_string()),
        K::PermissionDenied => Error::PermissionDenied(e.to_string()),
        K::UnsupportedConfig | K::UnsupportedOperation | K::HostUnavailable => {
            Error::Unsupported(e.to_string())
        }
        K::InvalidInput => Error::Invalid(e.to_string()),
        _ => Error::Other(e.to_string()),
    }
}

fn system_backend_name() -> &'static str {
    if cfg!(windows) {
        "wasapi_loopback"
    } else if cfg!(target_os = "linux") {
        "pulse"
    } else {
        "cpal"
    }
}

fn is_monitor(d: &cpal::Device) -> bool {
    d.id()
        .map(|i| i.id().ends_with(".monitor"))
        .unwrap_or(false)
}

fn device_name(d: &cpal::Device) -> String {
    d.description()
        .map(|desc| desc.name().to_string())
        .unwrap_or_else(|_| d.to_string())
}

fn list_devices_blocking() -> Result<Vec<AudioDeviceInfo>> {
    let host = cpal::default_host();
    let mut out = vec![];
    let default_mic = host.default_input_device().and_then(|d| d.id().ok());
    for d in host.input_devices().map_err(cpal_err)? {
        if is_monitor(&d) {
            continue;
        }
        let Ok(id) = d.id() else { continue };
        out.push(AudioDeviceInfo {
            id: id.to_string(),
            name: device_name(&d),
            channel: AudioChannel::Mic,
            backend: "cpal".into(),
            is_default: Some(&id) == default_mic.as_ref(),
        });
    }
    let default_sys = resolve_system_default(&host).ok().and_then(|d| d.id().ok());
    for d in system_candidates(&host)? {
        let Ok(id) = d.id() else { continue };
        out.push(AudioDeviceInfo {
            id: id.to_string(),
            name: device_name(&d),
            channel: AudioChannel::System,
            backend: system_backend_name().into(),
            is_default: Some(&id) == default_sys.as_ref(),
        });
    }
    Ok(out)
}

/// Devices that can serve as the `System` channel.
fn system_candidates(host: &cpal::Host) -> Result<Vec<cpal::Device>> {
    if cfg!(windows) {
        // WASAPI: any render endpoint can be captured in loopback mode.
        Ok(host.output_devices().map_err(cpal_err)?.collect())
    } else {
        Ok(host
            .input_devices()
            .map_err(cpal_err)?
            .filter(is_monitor)
            .collect())
    }
}

#[cfg(windows)]
fn resolve_system_default(host: &cpal::Host) -> Result<cpal::Device> {
    host.default_output_device()
        .ok_or_else(|| Error::NotFound("no default render device for loopback".into()))
}

#[cfg(target_os = "linux")]
fn resolve_system_default(host: &cpal::Host) -> Result<cpal::Device> {
    if host.id() != cpal::HostId::PulseAudio {
        return Err(Error::Unsupported(format!(
            "system audio capture needs a PulseAudio/pipewire-pulse server (host is {:?})",
            host.id()
        )));
    }
    let sink = host
        .default_output_device()
        .ok_or_else(|| Error::NotFound("no default sink".into()))?;
    let want = format!("{}.monitor", sink.id().map_err(cpal_err)?.id());
    let mut fallback = None;
    for d in host.input_devices().map_err(cpal_err)? {
        let Ok(id) = d.id() else { continue };
        if id.id() == want {
            return Ok(d);
        }
        if fallback.is_none() && id.id().ends_with(".monitor") {
            fallback = Some(d);
        }
    }
    fallback.ok_or_else(|| Error::NotFound(format!("monitor source {want} not found")))
}

#[cfg(not(any(windows, target_os = "linux")))]
fn resolve_system_default(_host: &cpal::Host) -> Result<cpal::Device> {
    Err(Error::Unsupported(
        "system audio capture is implemented for Windows and Linux only".into(),
    ))
}

fn resolve(host: &cpal::Host, channel: AudioChannel, id: Option<&str>) -> Result<cpal::Device> {
    if let Some(id) = id {
        let did = cpal::DeviceId::from_str(id).map_err(cpal_err)?;
        let h = cpal::host_from_id(did.host()).map_err(cpal_err)?;
        return h
            .device_by_id(&did)
            .ok_or_else(|| Error::NotFound(format!("audio device {id}")));
    }
    match channel {
        AudioChannel::System => resolve_system_default(host),
        AudioChannel::Mic | AudioChannel::Mixed => host
            .default_input_device()
            .ok_or_else(|| Error::NotFound("no default input device".into())),
    }
}

fn open_blocking(
    clock: CaptureClock,
    channel: AudioChannel,
    cfg: &CaptureConfig,
) -> Result<PipelineStream> {
    let (tx, mut stream) = channel_pair(channel, clock, cfg.frame_ms, AlignConfig::live());
    let stop = Arc::new(AtomicBool::new(false));
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<String>>();
    let device_id = cfg.device_id.clone();
    let stop2 = stop.clone();
    let handle = std::thread::Builder::new()
        .name(format!("almond-capture-{channel:?}").to_lowercase())
        .spawn(move || capture_loop(channel, device_id, tx, stop2, ready_tx))
        .map_err(Error::Io)?;
    match ready_rx.recv_timeout(Duration::from_secs(15)) {
        Ok(Ok(name)) => {
            tracing::info!(?channel, device = %name, "capture started");
        }
        Ok(Err(e)) => {
            let _ = handle.join();
            return Err(e);
        }
        Err(_) => {
            stop.store(true, Ordering::SeqCst);
            return Err(Error::Other(format!(
                "timed out opening {channel:?} capture device"
            )));
        }
    }
    stream.set_on_stop(move || stop.store(true, Ordering::SeqCst));
    Ok(stream)
}

struct Running {
    _stream: cpal::Stream,
    failed: Arc<AtomicBool>,
    key: String,
    name: String,
}

enum Exit {
    Stop,
    Reopen(&'static str),
}

fn capture_loop(
    channel: AudioChannel,
    device_id: Option<String>,
    tx: RawSender,
    stop: Arc<AtomicBool>,
    ready: std::sync::mpsc::Sender<Result<String>>,
) {
    let host = cpal::default_host();
    let mut ready = Some(ready);
    let mut backoff = Duration::from_millis(250);
    let mut last_err = String::new();
    while !stop.load(Ordering::SeqCst) && !tx.is_closed() {
        match start_stream(&host, channel, device_id.as_deref(), &tx) {
            Ok(running) => {
                match ready.take() {
                    Some(r) => {
                        let _ = r.send(Ok(running.name.clone()));
                    }
                    None => tx.warn(format!("{channel:?}: re-opened on {}", running.name)),
                }
                backoff = Duration::from_millis(250);
                last_err.clear();
                match supervise(&host, channel, device_id.is_none(), &running, &stop, &tx) {
                    Exit::Stop => break,
                    Exit::Reopen(why) => {
                        tracing::info!(?channel, "{why}; re-opening capture");
                        drop(running);
                    }
                }
            }
            Err(e) => {
                if let Some(r) = ready.take() {
                    let _ = r.send(Err(e));
                    return;
                }
                let msg = e.to_string();
                if msg != last_err {
                    tx.warn(format!("{channel:?}: waiting for device: {msg}"));
                    last_err = msg;
                }
                sleep_unless(&stop, backoff);
                backoff = (backoff * 2).min(Duration::from_secs(3));
            }
        }
    }
    tx.end();
}

fn sleep_unless(stop: &AtomicBool, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until && !stop.load(Ordering::SeqCst) {
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn supervise(
    host: &cpal::Host,
    channel: AudioChannel,
    follow_default: bool,
    r: &Running,
    stop: &AtomicBool,
    tx: &RawSender,
) -> Exit {
    let mut last_check = Instant::now();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if stop.load(Ordering::SeqCst) || tx.is_closed() {
            return Exit::Stop;
        }
        if r.failed.load(Ordering::SeqCst) {
            return Exit::Reopen("stream error");
        }
        if follow_default && last_check.elapsed() >= Duration::from_secs(2) {
            last_check = Instant::now();
            if let Ok(d) = resolve(host, channel, None)
                && let Ok(id) = d.id()
                && id.to_string() != r.key
            {
                return Exit::Reopen("default device changed");
            }
        }
    }
}

fn start_stream(
    host: &cpal::Host,
    channel: AudioChannel,
    device_id: Option<&str>,
    tx: &RawSender,
) -> Result<Running> {
    let device = resolve(host, channel, device_id)?;
    let key = device.id().map_err(cpal_err)?.to_string();
    let name = device_name(&device);
    // Inputs (incl. Pulse monitors) report an input config; WASAPI render endpoints used for
    // loopback only report an output config.
    let supported = device
        .default_input_config()
        .or_else(|_| device.default_output_config())
        .map_err(cpal_err)?;
    let failed = Arc::new(AtomicBool::new(false));
    let config = supported.config();
    let fmt = supported.sample_format();
    let stream = {
        use cpal::SampleFormat as F;
        let (d, c, t, f) = (&device, config, tx.clone(), failed.clone());
        match fmt {
            F::F32 => build::<f32>(d, c, t, f),
            F::F64 => build::<f64>(d, c, t, f),
            F::I16 => build::<i16>(d, c, t, f),
            F::I32 => build::<i32>(d, c, t, f),
            F::I24 => build::<cpal::I24>(d, c, t, f),
            F::I8 => build::<i8>(d, c, t, f),
            F::U8 => build::<u8>(d, c, t, f),
            F::U16 => build::<u16>(d, c, t, f),
            F::U32 => build::<u32>(d, c, t, f),
            other => Err(Error::Unsupported(format!("sample format {other:?}"))),
        }?
    };
    stream.play().map_err(cpal_err)?;
    tracing::debug!(
        ?channel,
        device = %name,
        rate = config.sample_rate,
        channels = config.channels,
        format = ?fmt,
        "stream playing"
    );
    Ok(Running {
        _stream: stream,
        failed,
        key,
        name,
    })
}

fn build<T>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    tx: RawSender,
    failed: Arc<AtomicBool>,
) -> Result<cpal::Stream>
where
    T: SizedSample + Send + 'static,
    f32: FromSample<T>,
{
    let rate = config.sample_rate;
    let ch = config.channels.max(1) as usize;
    let err_tx = tx.clone();
    let on_error = move |e: cpal::Error| {
        if matches!(e.kind(), cpal::ErrorKind::Xrun) {
            err_tx.warn("capture xrun");
        } else {
            err_tx.warn(format!("stream error: {e}"));
            failed.store(true, Ordering::SeqCst);
        }
    };
    let on_data = move |data: &[T], info: &cpal::InputCallbackInfo| {
        let ts = info.timestamp();
        let latency = ts
            .callback
            .checked_duration_since(ts.capture)
            .unwrap_or_default();
        let now = Instant::now();
        let at = now.checked_sub(latency).unwrap_or(now);
        let mono: Vec<f32> = if ch == 1 {
            data.iter().map(|s| f32::from_sample_(*s)).collect()
        } else {
            let inv = 1.0 / ch as f32;
            data.chunks_exact(ch)
                .map(|f| f.iter().map(|s| f32::from_sample_(*s)).sum::<f32>() * inv)
                .collect()
        };
        tx.push(at, rate, mono);
    };
    device
        .build_input_stream::<T, _, _>(config, on_data, on_error, Some(Duration::from_secs(5)))
        .map_err(cpal_err)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Needs real audio hardware: `cargo test -p almond-audio -- --ignored cpal`.
    #[test]
    #[ignore]
    fn cpal_lists_devices() {
        let devs = list_devices_blocking().unwrap();
        for d in &devs {
            println!(
                "{:?} {} [{}] default={}",
                d.channel, d.name, d.id, d.is_default
            );
        }
        assert!(!devs.is_empty());
    }
}
