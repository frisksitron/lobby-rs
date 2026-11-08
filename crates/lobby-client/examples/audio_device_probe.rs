//! Opens the default microphone and speakers the way Voice does (48 kHz, `BufferSize::Default`)
//! and reports what WASAPI gives: frames per callback, the time between callbacks, and cpal's
//! timestamps (microphone: callback minus capture; speakers: playback minus callback). Then the
//! same with `BufferSize::Fixed` at smaller sizes. The speakers play silence. Prints one JSON
//! object on stdout and a summary on stderr.
//!
//! ```text
//! cargo run --release -p lobby-client --example audio_device_probe -- --seconds 10
//! ```
//!
//! `--seconds N` is how long the default buffer runs (default 10); each fixed size runs
//! `--fixed-seconds N` (default 3).

use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use cpal::{
    BufferSize, SampleFormat, StreamConfig, SupportedBufferSize,
    traits::{DeviceTrait as _, HostTrait as _, StreamTrait as _},
};
use serde_json::{Value, json};

const SAMPLE_RATE: u32 = 48_000;
const FIXED_SIZES: [u32; 4] = [480, 240, 128, 64];
const START_TIMEOUT: Duration = Duration::from_secs(3);
/// Callbacks kept per stream: far more than a run makes.
const KEPT: usize = 100_000;

/// One callback: when it came, how many frames it carried, and cpal's delta for it.
#[derive(Debug, Clone, Copy)]
struct Call {
    at: Instant,
    frames: usize,
    delta: Duration,
}

#[derive(Default)]
struct Calls {
    calls: Vec<Call>,
    errors: Vec<String>,
}

type Shared = Arc<Mutex<Calls>>;

fn shared() -> Shared {
    Arc::new(Mutex::new(Calls {
        calls: Vec::with_capacity(KEPT),
        errors: Vec::new(),
    }))
}

fn note(calls: &Shared, frames: usize, delta: Duration) {
    if let Ok(mut c) = calls.try_lock()
        && c.calls.len() < KEPT
    {
        c.calls.push(Call {
            at: Instant::now(),
            frames,
            delta,
        });
    }
}

fn on_error(calls: Shared) -> impl FnMut(cpal::Error) + Send + 'static {
    move |e| calls.lock().expect("calls").errors.push(e.to_string())
}

fn buffer_size(b: &SupportedBufferSize) -> Value {
    match b {
        SupportedBufferSize::Range { min, max } => json!({ "min": min, "max": max }),
        SupportedBufferSize::Unknown => json!("unknown"),
    }
}

fn name(device: &cpal::Device) -> String {
    device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "unnamed".into())
}

/// The microphone's config as Voice picks it: its default, at 48 kHz when it offers that.
fn input_config(device: &cpal::Device) -> Result<(StreamConfig, SampleFormat), String> {
    let default = device.default_input_config().map_err(|e| e.to_string())?;
    let format = default.sample_format();
    let mut config = default.config();
    if config.sample_rate != SAMPLE_RATE
        && let Ok(mut configs) = device.supported_input_configs()
        && let Some(c) = configs.find(|c| {
            c.channels() == config.channels
                && c.sample_format() == format
                && (c.min_sample_rate()..=c.max_sample_rate()).contains(&SAMPLE_RATE)
        })
    {
        config = c.with_sample_rate(SAMPLE_RATE).config();
    }
    Ok((config, format))
}

fn open_input(
    device: &cpal::Device,
    buffer: BufferSize,
    calls: &Shared,
) -> Result<(cpal::Stream, StreamConfig), String> {
    let (mut config, format) = input_config(device)?;
    config.buffer_size = buffer;
    let channels = usize::from(config.channels.max(1));
    let stream = match format {
        SampleFormat::F32 => {
            let noted = calls.clone();
            device.build_input_stream::<f32, _, _>(
                config,
                move |data, info| {
                    let t = info.timestamp();
                    note(
                        &noted,
                        data.len() / channels,
                        t.callback.duration_since(t.capture),
                    );
                },
                on_error(calls.clone()),
                Some(START_TIMEOUT),
            )
        }
        SampleFormat::I16 => {
            let noted = calls.clone();
            device.build_input_stream::<i16, _, _>(
                config,
                move |data, info| {
                    let t = info.timestamp();
                    note(
                        &noted,
                        data.len() / channels,
                        t.callback.duration_since(t.capture),
                    );
                },
                on_error(calls.clone()),
                Some(START_TIMEOUT),
            )
        }
        other => return Err(format!("unsupported microphone format {other:?}")),
    }
    .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, config))
}

fn open_output(
    device: &cpal::Device,
    buffer: BufferSize,
    calls: &Shared,
) -> Result<(cpal::Stream, StreamConfig), String> {
    let default = device.default_output_config().map_err(|e| e.to_string())?;
    let config = StreamConfig {
        channels: default.channels(),
        sample_rate: SAMPLE_RATE,
        buffer_size: buffer,
    };
    let channels = usize::from(config.channels.max(1));
    let noted = calls.clone();
    let stream = device
        .build_output_stream::<f32, _, _>(
            config,
            move |data, info| {
                data.fill(0.0);
                let t = info.timestamp();
                note(
                    &noted,
                    data.len() / channels,
                    t.playback.duration_since(t.callback),
                );
            },
            on_error(calls.clone()),
            Some(START_TIMEOUT),
        )
        .map_err(|e| e.to_string())?;
    stream.play().map_err(|e| e.to_string())?;
    Ok((stream, config))
}

fn ms(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0 * 100.0).round() / 100.0
}

/// Min, quartiles and max of some numbers, and the 10th and 90th percentiles.
fn spread(mut v: Vec<f64>) -> Value {
    v.sort_by(f64::total_cmp);
    if v.is_empty() {
        return Value::Null;
    }
    let at = |p: usize| v[(v.len() - 1) * p / 100];
    json!({
        "min": v[0],
        "p10": at(10),
        "p25": at(25),
        "median": at(50),
        "p75": at(75),
        "p90": at(90),
        "max": v[v.len() - 1],
    })
}

/// The first callbacks fill the device's buffer: they say little about the steady state.
const SETTLE: usize = 10;

fn describe(stream: &cpal::Stream, config: &StreamConfig, calls: &Shared) -> Value {
    let calls = calls.lock().expect("calls");
    let steady = calls.calls.get(SETTLE..).unwrap_or_default();
    let gaps: Vec<f64> = steady.windows(2).map(|w| ms(w[1].at - w[0].at)).collect();
    json!({
        "opened": true,
        "channels": config.channels,
        "sample_rate": config.sample_rate,
        "stream_buffer_size_frames": stream.buffer_size().ok(),
        "callbacks": calls.calls.len(),
        "first_callbacks_frames": calls.calls.iter().take(SETTLE).map(|c| c.frames).collect::<Vec<_>>(),
        "frames_per_callback": spread(steady.iter().map(|c| c.frames as f64).collect()),
        "callback_interval_ms": spread(gaps),
        "timestamp_delta_ms": spread(steady.iter().map(|c| ms(c.delta)).collect()),
        "errors": calls.errors,
    })
}

/// Both streams at `buffer` for `seconds`, the microphone and the speakers at once as in a call.
fn trial(input: &cpal::Device, output: &cpal::Device, buffer: BufferSize, seconds: u64) -> Value {
    let (mic_calls, out_calls) = (shared(), shared());
    let mic = open_input(input, buffer, &mic_calls);
    let speakers = open_output(output, buffer, &out_calls);
    std::thread::sleep(Duration::from_secs(seconds));
    let side = |opened: &Result<(cpal::Stream, StreamConfig), String>, calls: &Shared| match opened
    {
        Ok((stream, config)) => describe(stream, config, calls),
        Err(e) => json!({ "opened": false, "error": e }),
    };
    let report = json!({
        "buffer_size": format!("{buffer:?}"),
        "seconds": seconds,
        "microphone": side(&mic, &mic_calls),
        "speakers": side(&speakers, &out_calls),
    });
    drop(speakers);
    drop(mic);
    report
}

fn summary_line(trial: &Value) -> String {
    let side = |s: &Value| {
        if s["opened"] != json!(true) {
            return format!("failed: {}", s["error"]);
        }
        format!(
            "frames/callback {}..{} (median {}), every {} ms, delta ms min {} p10 {} p25 {} \
             median {} p75 {} p90 {} max {}",
            s["frames_per_callback"]["min"],
            s["frames_per_callback"]["max"],
            s["frames_per_callback"]["median"],
            s["callback_interval_ms"]["median"],
            s["timestamp_delta_ms"]["min"],
            s["timestamp_delta_ms"]["p10"],
            s["timestamp_delta_ms"]["p25"],
            s["timestamp_delta_ms"]["median"],
            s["timestamp_delta_ms"]["p75"],
            s["timestamp_delta_ms"]["p90"],
            s["timestamp_delta_ms"]["max"],
        )
    };
    format!(
        "{}\n  microphone: {}\n  speakers:   {}",
        trial["buffer_size"],
        side(&trial["microphone"]),
        side(&trial["speakers"]),
    )
}

fn main() {
    let mut seconds = 10;
    let mut fixed_seconds = 3;
    let mut args = std::env::args().skip(1);
    while let Some(flag) = args.next() {
        let value = args.next().and_then(|v| v.parse::<u64>().ok());
        match (flag.as_str(), value) {
            ("--seconds", Some(n)) => seconds = n,
            ("--fixed-seconds", Some(n)) => fixed_seconds = n,
            _ => {
                eprintln!("usage: audio_device_probe [--seconds N] [--fixed-seconds N]");
                std::process::exit(2);
            }
        }
    }
    let host = cpal::default_host();
    let (Some(input), Some(output)) = (host.default_input_device(), host.default_output_device())
    else {
        eprintln!("no default microphone or speakers");
        std::process::exit(1);
    };
    let input_default = input.default_input_config().ok();
    let output_default = output.default_output_config().ok();
    let ranges = |configs: Vec<cpal::SupportedStreamConfigRange>| -> Vec<Value> {
        configs
            .iter()
            .map(|c| {
                json!({
                    "channels": c.channels(),
                    "format": format!("{:?}", c.sample_format()),
                    "rates": [c.min_sample_rate(), c.max_sample_rate()],
                    "buffer_size": buffer_size(c.buffer_size()),
                })
            })
            .collect()
    };
    let devices = json!({
        "host": format!("{:?}", host.id()),
        "microphone": {
            "name": name(&input),
            "default": input_default.as_ref().map(|c| json!({
                "channels": c.channels(),
                "rate": c.sample_rate(),
                "format": format!("{:?}", c.sample_format()),
                "buffer_size": buffer_size(c.buffer_size()),
            })),
            "supported": input.supported_input_configs().map(|c| ranges(c.collect())).ok(),
        },
        "speakers": {
            "name": name(&output),
            "default": output_default.as_ref().map(|c| json!({
                "channels": c.channels(),
                "rate": c.sample_rate(),
                "format": format!("{:?}", c.sample_format()),
                "buffer_size": buffer_size(c.buffer_size()),
            })),
            "supported": output.supported_output_configs().map(|c| ranges(c.collect())).ok(),
        },
    });
    let mut trials = vec![trial(&input, &output, BufferSize::Default, seconds)];
    for frames in FIXED_SIZES {
        trials.push(trial(
            &input,
            &output,
            BufferSize::Fixed(frames),
            fixed_seconds,
        ));
    }
    eprintln!(
        "microphone {}, speakers {}",
        devices["microphone"]["name"], devices["speakers"]["name"]
    );
    for t in &trials {
        eprintln!("{}", summary_line(t));
    }
    println!("{}", json!({ "devices": devices, "trials": trials }));
}
