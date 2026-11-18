//! Bluetooth headphones as sound outputs. Paired headphones that aren't connected (say the
//! iPhone has the AirPods) have no PipeWire sink, so Kodi can't list them. This module finds
//! them through BlueZ on the system D-Bus and connects them on request; once connected,
//! PipeWire adds a `bluez_output.<address>` sink and Kodi offers it like any other output.
//! It also moves Kodi's sound back to chosen headphones after they drop out and return.

use std::{
    collections::HashMap,
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Deserialize;
use zbus::{Connection, DeserializeDict, OwnedObjectPath, Type};

use crate::{kodi::Kodi, listen};

/// The A2DP "Audio Sink" service: the device plays audio, i.e. headphones or a speaker.
const AUDIO_SINK_UUID: &str = "0000110b-0000-1000-8000-00805f9b34fb";
/// How long BlueZ gets to connect; headphones held by another device can take a few seconds.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
/// How often [`Bluetooth::switch_back`] checks whether Kodi fell back from the headphones.
const SWITCH_BACK_EVERY: Duration = Duration::from_secs(3);

/// A Bluetooth device address in BlueZ's form, e.g. `A0:B1:C2:D3:E4:F5`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct Address(String);

impl TryFrom<String> for Address {
    type Error = &'static str;

    fn try_from(s: String) -> Result<Self, Self::Error> {
        let octets = s.split(':').collect::<Vec<_>>();
        let valid =
            octets.len() == 6 && octets.iter().all(|o| o.len() == 2 && o.chars().all(|c| c.is_ascii_hexdigit()));
        if valid { Ok(Self(s.to_ascii_uppercase())) } else { Err("not a Bluetooth address") }
    }
}

impl Address {
    /// Whether a Kodi audio device id is this device's PipeWire sink, e.g.
    /// `PIPEWIRE:bluez_output.A0_B1_C2_D3_E4_F5.1|AirPods`.
    pub fn is_output(&self, kodi_device: &str) -> bool {
        kodi_device.contains(&format!("bluez_output.{}.", self.0.replace(':', "_")))
    }

    /// The Bluetooth device behind a Kodi audio device id, if it is one.
    fn from_output(kodi_device: &str) -> Option<Self> {
        let (_, rest) = kodi_device.split_once("bluez_output.")?;
        let raw = rest.split('.').next()?;
        Self::try_from(raw.replace('_', ":")).ok()
    }
}

/// Whether a Kodi audio device id is its "Default Output Device", which is where Kodi
/// falls back to when the selected device disappears.
fn is_default(kodi_device: &str) -> bool {
    kodi_device.split('|').next().is_some_and(|d| d.ends_with(":Default"))
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Paired headphones (or a speaker) known to BlueZ.
#[derive(Debug)]
pub struct Headphones {
    pub address: Address,
    /// The name shown in Bluetooth settings (BlueZ's alias, which falls back to the device's name).
    pub name: String,
    pub connected: bool,
    path: OwnedObjectPath,
}

#[derive(Debug, thiserror::Error)]
pub enum BluetoothError {
    #[error("Bluetooth isn't available on the PC")]
    Unavailable,
    #[error("Those headphones aren't paired with the PC")]
    NotPaired,
    #[error("Couldn't connect to {0}. Are they out of the case and nearby?")]
    Connect(String),
    /// Connected, but Kodi didn't list the new sink in time; Kodi may only look for outputs
    /// when it starts.
    #[error("{0} connected, but Kodi doesn't list them yet. Restart Kodi and try again.")]
    NotInKodi(String),
    #[error(transparent)]
    Bus(#[from] zbus::Error),
}

/// The `org.bluez.Device1` properties this module reads. Unknown keys are ignored.
#[derive(Debug, Default, DeserializeDict, Type)]
#[zbus(signature = "a{sv}", rename_all = "PascalCase")]
struct DeviceProps {
    address: Option<String>,
    alias: Option<String>,
    paired: Option<bool>,
    connected: Option<bool>,
    #[zbus(rename = "UUIDs")]
    uuids: Option<Vec<String>>,
}

/// One object's interfaces as returned by `GetManagedObjects`; only devices matter here.
#[derive(Debug, Default, DeserializeDict, Type)]
#[zbus(signature = "a{sa{sv}}")]
struct Interfaces {
    #[zbus(rename = "org.bluez.Device1")]
    device: Option<DeviceProps>,
}

/// Handle to BlueZ, plus the headphones last chosen in the remote. Without a system bus
/// (e.g. in a container) every BlueZ call fails with [`BluetoothError::Unavailable`] and the
/// remote simply offers no Bluetooth outputs.
#[derive(Clone)]
pub struct Bluetooth {
    bus: Option<Connection>,
    chosen: Arc<Mutex<Option<Address>>>,
}

impl Bluetooth {
    pub async fn new() -> Self {
        let bus =
            Connection::system().await.inspect_err(|e| tracing::warn!("no system D-Bus, so no Bluetooth: {e}")).ok();
        Self { bus, chosen: Default::default() }
    }

    /// Records an output chosen on purpose. Headphones are remembered for
    /// [`Bluetooth::switch_back`]; anything else, including "System Default", forgets them.
    pub fn remember_choice(&self, kodi_device: &str) {
        *self.chosen.lock().expect("lock poisoned") = Address::from_output(kodi_device);
    }

    /// Runs forever. When the chosen headphones drop out (another device takes them, they go
    /// out of range, their codec changes), Kodi falls back to its default output and stays
    /// there. Once they're back, this moves Kodi's sound to them again.
    ///
    /// The choice is learned by watching Kodi's output, so it counts however it was made (the
    /// remote, Kodi's own settings, the listen watchdog) and survives a restart of the remote.
    /// Kodi sitting on its default output is read as its fallback, which is why choosing
    /// "System Default" in the remote goes through [`Bluetooth::remember_choice`] instead.
    pub async fn switch_back(self, kodi: Kodi) {
        loop {
            tokio::time::sleep(SWITCH_BACK_EVERY).await;
            // The whole list, not just `audio_output`: the target and the hop come from it.
            let Ok(outputs) = kodi.audio_outputs().await else { continue };
            if !is_default(&outputs.current) {
                self.remember_choice(&outputs.current);
                continue;
            }
            let Some(address) = self.chosen.lock().expect("lock poisoned").clone() else { continue };
            let devices = || outputs.options.iter().filter_map(|o| o.value.as_str());
            let Some(target) = devices().find(|d| address.is_output(d)) else { continue };
            // The fallback leaves the headphones as the stored setting, so selecting them again
            // is no change to Kodi and it stays on the default output. Going through another
            // output first makes it a real change. The listen stream sink is silent, so it's the
            // preferred hop.
            let hop = devices()
                .find(|d| listen::is_stream_device(d))
                .or_else(|| devices().find(|d| !is_default(d) && *d != target));
            if let Some(hop) = hop {
                if let Err(e) = kodi.set_audio_output(hop).await {
                    tracing::warn!("switching back to {address}: {e}");
                    continue;
                }
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            match kodi.set_audio_output(target).await {
                Ok(()) => tracing::info!("{address} is back; sound moved to {target}"),
                Err(e) => tracing::warn!("switching back to {address}: {e}"),
            }
        }
    }

    /// Every paired device that plays audio, connected or not.
    pub async fn headphones(&self) -> Result<Vec<Headphones>, BluetoothError> {
        let bus = self.bus.as_ref().ok_or(BluetoothError::Unavailable)?;
        let reply = bus
            .call_method(Some("org.bluez"), "/", Some("org.freedesktop.DBus.ObjectManager"), "GetManagedObjects", &())
            .await?;
        let objects: HashMap<OwnedObjectPath, Interfaces> = reply.body().deserialize()?;
        let headphones = objects
            .into_iter()
            .filter_map(|(path, interfaces)| {
                let device = interfaces.device?;
                let plays_audio = device.uuids?.iter().any(|u| u.eq_ignore_ascii_case(AUDIO_SINK_UUID));
                if !device.paired.unwrap_or(false) || !plays_audio {
                    return None;
                }
                let address = Address::try_from(device.address?).ok()?;
                let name = device.alias.unwrap_or_else(|| address.to_string());
                Some(Headphones { address, name, connected: device.connected.unwrap_or(false), path })
            })
            .collect();
        Ok(headphones)
    }

    /// Connects paired headphones, taking them over from another device if need be, and
    /// returns their name. Already-connected headphones return straight away.
    pub async fn connect(&self, address: &Address) -> Result<String, BluetoothError> {
        let bus = self.bus.as_ref().ok_or(BluetoothError::Unavailable)?;
        let device =
            self.headphones().await?.into_iter().find(|h| &h.address == address).ok_or(BluetoothError::NotPaired)?;
        if device.connected {
            return Ok(device.name);
        }
        let call = bus.call_method(Some("org.bluez"), &device.path, Some("org.bluez.Device1"), "Connect", &());
        match tokio::time::timeout(CONNECT_TIMEOUT, call).await {
            Ok(Ok(_)) => Ok(device.name),
            Ok(Err(e)) => {
                tracing::warn!("connecting {address}: {e}");
                Err(BluetoothError::Connect(device.name))
            }
            Err(_) => Err(BluetoothError::Connect(device.name)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_accepts_bluez_form_only() {
        assert!(Address::try_from("a0:b1:c2:d3:e4:f5".to_owned()).is_ok_and(|a| a.to_string() == "A0:B1:C2:D3:E4:F5"));
        assert!(Address::try_from("A0:B1:C2:D3:E4".to_owned()).is_err());
        assert!(Address::try_from("A0:B1:C2:D3:E4:FG".to_owned()).is_err());
        assert!(Address::try_from("../../etc".to_owned()).is_err());
    }

    #[test]
    fn address_from_kodi_device_id() {
        let address = Address::from_output("PIPEWIRE:bluez_output.A0_B1_C2_D3_E4_F5.1|AirPods");
        assert_eq!(address.map(|a| a.to_string()).as_deref(), Some("A0:B1:C2:D3:E4:F5"));
        assert!(Address::from_output("PIPEWIRE:alsa_output.pci-0000_04_00.1.hdmi-stereo|TV").is_none());
        assert!(Address::from_output("PIPEWIRE:Default|Default Output Device (PIPEWIRE)").is_none());
    }

    #[test]
    fn address_matches_its_pipewire_sink() {
        let address = Address::try_from("A0:B1:C2:D3:E4:F5".to_owned()).unwrap();
        assert!(address.is_output("PIPEWIRE:bluez_output.A0_B1_C2_D3_E4_F5.1|AirPods"));
        assert!(!address.is_output("PIPEWIRE:bluez_output.A0_B1_C2_D3_E4_F6.1|Other"));
        assert!(!address.is_output("PIPEWIRE:alsa_output.pci-0000_04_00.1.hdmi-stereo|TV"));
    }
}
