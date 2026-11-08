//! Reads the default microphone's Windows volume, sets it to the value it already has, and prints
//! the change notices Windows sends back. The volume is never left changed.
//!
//! ```text
//! cargo run -p lobby-client --example mic_volume_probe
//! ```

use std::time::{Duration, Instant};

use lobby_client::{DeviceChoice, DeviceSource as _, MicGain, MicVolume, SystemDevices};

/// How long the probe waits for the control to open, and then for notices.
const OPEN_WITHIN: Duration = Duration::from_secs(3);
const NOTICES_FOR: Duration = Duration::from_secs(1);
const LOOK_EVERY: Duration = Duration::from_millis(10);

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter("lobby_client::media::mic_volume=debug")
        .init();
    let volume = MicVolume::start();
    volume.follow(&DeviceChoice::SystemDefault, &SystemDevices.list().inputs);
    let asked = Instant::now();
    while volume.read() == MicGain::Reading && asked.elapsed() < OPEN_WITHIN {
        std::thread::sleep(LOOK_EVERY);
    }
    let MicGain::Level(level) = volume.read() else {
        println!("the default microphone's volume: {:?}", volume.read());
        return;
    };
    println!(
        "the default microphone's volume: {level:.4}, read in {:?}",
        asked.elapsed()
    );
    volume.set(level);
    std::thread::sleep(NOTICES_FOR);
    println!("after setting it to the same value: {:?}", volume.read());
}
