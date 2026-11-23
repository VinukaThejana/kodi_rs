//! The system's sound outputs: PipeWire sinks, through pipewire-pulse's `pactl`. In browser
//! mode they stand in for Kodi's audio output setting, since Chrome plays to the default sink.
//!
//! Sinks are given Kodi's device-id form, `PIPEWIRE:<sink>|<description>`, so the audio sheet
//! names and matches them (the TV, Bluetooth headphones, the listen stream) exactly as it does
//! Kodi's outputs.

use serde::Deserialize;
use serde_json::Value;
use tokio::process::Command;

use crate::kodi::{AudioOutputs, SettingOption};

#[derive(Debug, thiserror::Error)]
#[error("couldn't change the sound output: {0}")]
pub struct SoundError(String);

#[derive(Deserialize)]
struct Sink {
    name: String,
    description: String,
}

/// Every output, and the default one as `current`.
pub async fn outputs() -> Result<AudioOutputs, SoundError> {
    let sinks: Vec<Sink> = json(&pactl(&["-f", "json", "list", "sinks"]).await?)?;
    let default = pactl(&["get-default-sink"]).await?;
    let id = |s: &Sink| format!("PIPEWIRE:{}|{}", s.name, s.description);
    Ok(AudioOutputs {
        current: sinks.iter().find(|s| s.name == default.trim()).map(id).unwrap_or_default(),
        options: sinks.iter().map(|s| SettingOption { label: s.description.clone(), value: Value::String(id(s)) }).collect(),
    })
}

/// Makes `device`, an id from [`outputs`], the default output, and moves what's playing onto
/// it: streams follow the default unless something pinned them to a sink.
pub async fn set_output(device: &str) -> Result<(), SoundError> {
    let known = outputs().await?.options.iter().any(|o| o.value.as_str() == Some(device));
    let sink = sink_name(device).filter(|_| known).ok_or_else(|| SoundError(format!("there's no output {device}")))?;
    pactl(&["set-default-sink", sink]).await?;
    #[derive(Deserialize)]
    struct Playing {
        index: u32,
    }
    let playing: Vec<Playing> = json(&pactl(&["-f", "json", "list", "sink-inputs"]).await?)?;
    for p in playing {
        pactl(&["move-sink-input", &p.index.to_string(), sink]).await?;
    }
    Ok(())
}

/// The sink in a device id from [`outputs`].
fn sink_name(device: &str) -> Option<&str> {
    device.strip_prefix("PIPEWIRE:")?.split('|').next().filter(|s| !s.is_empty())
}

fn json<T: serde::de::DeserializeOwned>(s: &str) -> Result<T, SoundError> {
    serde_json::from_str(s).map_err(|e| SoundError(format!("reading pactl's output: {e}")))
}

async fn pactl(args: &[&str]) -> Result<String, SoundError> {
    let out = Command::new("pactl").args(args).output().await.map_err(|e| SoundError(e.to_string()))?;
    if !out.status.success() {
        return Err(SoundError(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_sink_out_of_a_device_id() {
        assert_eq!(sink_name("PIPEWIRE:bluez_output.A0_B1_C2_D3_E4_F5.1|AirPods"), Some("bluez_output.A0_B1_C2_D3_E4_F5.1"));
        assert_eq!(sink_name("PIPEWIRE:kodi_remote|Kodi Remote stream"), Some("kodi_remote"));
        assert_eq!(sink_name("ALSA:hw:0,3"), None);
        assert_eq!(sink_name("PIPEWIRE:|nothing"), None);
    }
}
