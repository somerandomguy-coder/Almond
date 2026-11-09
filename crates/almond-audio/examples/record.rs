//! Record mic + system audio to Opus files and print live levels.
//!
//! ```text
//! cargo run -p almond-audio --example record -- --seconds 20 --out ./rec
//!     [--codec opus|wav] [--tone] [--list] [--mic <device-id>] [--system <device-id>]
//! ```
//! `--tone` plays a 660 Hz beep pattern on the default output during the run, so the loopback
//! channel can be verified without starting a call. Files land in `<out>/session-<unix ms>/`
//! (`mic.opus`, `system.opus`, `session.json`).

use almond_audio::level::LevelMeter;
use almond_audio::recorder::{Codec, RecorderConfig, SessionRecorder};
use almond_audio::{CaptureSession, CpalBackend};
use almond_types::audio::{AudioBackend, AudioStream, CaptureConfig};
use almond_types::meeting::AudioChannel;
use almond_types::mock::MemBlobStore;
use std::path::PathBuf;
use std::time::{Duration, Instant};

struct Args {
    seconds: u64,
    out: PathBuf,
    codec: Codec,
    tone: bool,
    list: bool,
    mic: Option<String>,
    system: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        seconds: 20,
        out: PathBuf::from("recordings"),
        codec: Codec::Opus,
        tone: false,
        list: false,
        mic: None,
        system: None,
    };
    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--seconds" => a.seconds = it.next().and_then(|v| v.parse().ok()).expect("--seconds N"),
            "--out" => a.out = it.next().expect("--out DIR").into(),
            "--codec" => {
                a.codec = match it.next().as_deref() {
                    Some("wav") => Codec::Wav,
                    _ => Codec::Opus,
                }
            }
            "--tone" => a.tone = true,
            "--list" => a.list = true,
            "--mic" => a.mic = it.next(),
            "--system" => a.system = it.next(),
            "-h" | "--help" => {
                println!(
                    "record [--seconds N] [--out DIR] [--codec opus|wav] [--tone] [--list] [--mic ID] [--system ID]"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    a
}

/// Plays a beep pattern (0.4 s on / 0.6 s off, 660 Hz, -14 dBFS) on the default output device.
fn start_tone() -> Option<cpal::Stream> {
    use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
    let dev = cpal::default_host().default_output_device()?;
    let sc = dev.default_output_config().ok()?;
    if sc.sample_format() != cpal::SampleFormat::F32 {
        eprintln!(
            "--tone: output format {:?} not supported",
            sc.sample_format()
        );
        return None;
    }
    let cfg = sc.config();
    let rate = cfg.sample_rate as f32;
    let ch = cfg.channels as usize;
    let mut n: u64 = 0;
    let stream = dev
        .build_output_stream::<f32, _, _>(
            cfg,
            move |out: &mut [f32], _| {
                for frame in out.chunks_mut(ch) {
                    let t = n as f32 / rate;
                    let on = (t % 1.0) < 0.4;
                    let v = if on {
                        0.2 * (2.0 * std::f32::consts::PI * 660.0 * t).sin()
                    } else {
                        0.0
                    };
                    frame.fill(v);
                    n += 1;
                }
            },
            |e| eprintln!("tone stream error: {e}"),
            None,
        )
        .ok()?;
    stream.play().ok()?;
    Some(stream)
}

/// Beep-onset detector (10 ms windows, adaptive noise floor) used to measure the mic-vs-system
/// offset over time when `--tone` is on: the loopback hears the beep at the mix point, the mic
/// hears it after output latency + air, so the offset should be a positive constant. Any trend
/// across the run is clock drift between the channels.
#[derive(Default)]
struct Onsets {
    floor: Option<f32>,
    quiet_ms: u64,
    times: Vec<f64>,
}

impl Onsets {
    fn feed(&mut self, t0_ms: u64, samples: &[f32]) {
        for (k, w) in samples.chunks(160).enumerate() {
            let db = almond_audio::level::rms_dbfs(w);
            let floor = *self.floor.get_or_insert(db);
            let thr = floor.max(-90.0) + 15.0;
            if db > thr {
                if self.quiet_ms >= 200 {
                    let amp = 10f32.powf(thr / 20.0);
                    let idx = w.iter().position(|x| x.abs() > amp).unwrap_or(0);
                    self.times
                        .push(t0_ms as f64 + k as f64 * 10.0 + idx as f64 / 16.0);
                }
                self.quiet_ms = 0;
            } else {
                self.quiet_ms += 10;
                let f = if db < floor {
                    db
                } else {
                    0.95 * floor + 0.05 * db
                };
                self.floor = Some(f);
            }
        }
    }
}

fn report_alignment(mic: &[f64], sys: &[f64]) {
    let deltas: Vec<(f64, f64)> = sys
        .iter()
        .filter_map(|&s| {
            mic.iter()
                .find(|&&m| m >= s - 50.0 && m <= s + 400.0)
                .map(|&m| (s, m - s))
        })
        .collect();
    println!(
        "\nAlignment (tone onsets): system {} / mic {} / matched {}",
        sys.len(),
        mic.len(),
        deltas.len()
    );
    if deltas.len() < 3 {
        println!("  not enough matched beeps (is the speaker audible to the mic?)");
        return;
    }
    let stats = |v: &[(f64, f64)]| {
        let n = v.len() as f64;
        let mean = v.iter().map(|d| d.1).sum::<f64>() / n;
        let min = v.iter().map(|d| d.1).fold(f64::MAX, f64::min);
        let max = v.iter().map(|d| d.1).fold(f64::MIN, f64::max);
        (mean, min, max)
    };
    if std::env::var_os("ALMOND_DEBUG_ONSETS").is_some() {
        for (s, d) in &deltas {
            println!("    beep at {:>9.1} ms: mic {d:+.1} ms", s);
        }
    }
    let (mean, min, max) = stats(&deltas);
    let third = (deltas.len() / 3).max(1);
    let (first, _, _) = stats(&deltas[..third]);
    let (last, _, _) = stats(&deltas[deltas.len() - third..]);
    println!(
        "  mic lags system by {mean:.1} ms (min {min:.1}, max {max:.1}); first third {first:.1} ms, last third {last:.1} ms -> drift {:+.1} ms over {:.0} s",
        last - first,
        (deltas.last().unwrap().0 - deltas[0].0) / 1000.0
    );
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "almond_audio=info".into()),
        )
        .with_writer(std::io::stderr)
        .init();
    let backend = CpalBackend::new();

    let devices = backend.list_devices().await?;
    println!("Devices:");
    for d in &devices {
        println!(
            "  {:<6} {} {:<60} [{}] ({})",
            format!("{:?}", d.channel),
            if d.is_default { "*" } else { " " },
            d.name,
            d.id,
            d.backend
        );
    }
    if args.list {
        return Ok(());
    }

    let clock = backend.reset_clock();
    let mk = |id: Option<String>| CaptureConfig {
        device_id: id,
        frame_ms: 100,
    };
    let mic = backend.open_stream(AudioChannel::Mic, &mk(args.mic.clone()))?;
    let sys = backend.open_stream(AudioChannel::System, &mk(args.system.clone()))?;
    let stats = [
        (AudioChannel::Mic, mic.stats()),
        (AudioChannel::System, sys.stats()),
    ];

    let dir = args.out.join(format!("session-{}", clock.epoch_unix_ms()));
    let rec = SessionRecorder::create(
        &dir,
        clock.epoch_unix_ms(),
        RecorderConfig {
            codec: args.codec,
            chunk_secs: 60,
            keep_files: true,
            ..Default::default()
        },
    )?;
    let streams: Vec<Box<dyn AudioStream>> = vec![Box::new(mic), Box::new(sys)];
    let (session, mut live) = CaptureSession::start(streams, Some(rec), 512)?;

    let _tone = if args.tone { start_tone() } else { None };
    if args.tone {
        println!("Playing test tone on the default output.");
    }
    println!("Recording {} s into {} ...", args.seconds, dir.display());

    let mut total = [LevelMeter::default(), LevelMeter::default()];
    let mut second = [LevelMeter::default(), LevelMeter::default()];
    let mut last_t0 = [0u64, 0u64];
    let mut onsets = [Onsets::default(), Onsets::default()];
    let mut echo = almond_audio::echo::EchoDetector::default();
    let start = Instant::now();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(args.seconds);
    let every = Duration::from_secs((args.seconds / 30).max(1));
    let mut next_print = every;
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            f = live.recv() => {
                let Some(f) = f else { break };
                let i = usize::from(f.channel == AudioChannel::System);
                total[i].add(&f.samples);
                second[i].add(&f.samples);
                last_t0[i] = f.t0_ms;
                if args.tone {
                    onsets[i].feed(f.t0_ms, &f.samples);
                }
                echo.push(&f);
            }
        }
        if start.elapsed() >= next_print {
            println!(
                "  t={:>4}s  mic {:>6.1} dBFS (peak {:>6.1})   system {:>6.1} dBFS (peak {:>6.1})   t0 mic={} sys={}",
                next_print.as_secs(),
                second[0].rms_dbfs(),
                second[0].peak_dbfs(),
                second[1].rms_dbfs(),
                second[1].peak_dbfs(),
                last_t0[0],
                last_t0[1],
            );
            second = [LevelMeter::default(), LevelMeter::default()];
            next_print += every;
        }
    }
    drop(_tone);
    if args.tone {
        report_alignment(&onsets[0].times, &onsets[1].times);
    }
    match echo.estimate() {
        Some(e) => println!(
            "Speaker bleed detected: mic hears system audio {} ms later at {:.1} dB (r = {:.2}); headphones/AEC advised",
            e.delay_ms, e.coupling_db, e.correlation
        ),
        None => println!("No speaker bleed detected in the mic channel."),
    }

    let blobs = MemBlobStore::default();
    let summary = session.stop(&blobs).await?;
    println!("\nSummary:");
    for (i, name) in ["mic", "system"].iter().enumerate() {
        let st = stats[i].1.snapshot();
        println!(
            "  {name:<6} rms {:>6.1} dBFS  peak {:>6.1} dBFS  active {}/{} frames  | zero-filled {} ms, dropped {} ms, corrections {}, chunks {}, overflow {}, warnings {}",
            total[i].rms_dbfs(),
            total[i].peak_dbfs(),
            total[i].active_frames,
            total[i].frames,
            st.zero_filled_ms,
            st.dropped_ms,
            st.corrections,
            st.chunks,
            st.overflow,
            st.warnings,
        );
    }
    for t in &summary.tracks {
        println!(
            "  track {:?}: {} ms, {} @ {} Hz, blob {}",
            t.channel, t.duration_ms, t.codec, t.sample_rate, t.blob.0
        );
    }
    if !summary.errors.is_empty() {
        println!("  errors: {:?}", summary.errors);
    }
    println!("  live frames dropped: {}", summary.live_frames_dropped);
    println!("Files in {}", dir.display());
    Ok(())
}
