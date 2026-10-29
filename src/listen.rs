//! "Listen on this device": Kodi plays into the `kodi_remote` PipeWire sink, go2rtc streams
//! that sink to the browser over WebRTC, and this module brokers the WebRTC handshake, keeps
//! the sound in step with the TV picture ("Sync"), and puts Kodi's sound back where it was
//! once nobody is listening any more.

use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicI32, Ordering},
    },
    time::Duration,
};

use serde::Deserialize;

use crate::kodi::{Kodi, KodiError};

/// Marks Kodi's id for the stream sink, e.g. "PIPEWIRE:kodi_remote|Kodi Remote stream".
const STREAM_DEVICE: &str = "PIPEWIRE:kodi_remote";
/// go2rtc stream name, from deploy/go2rtc.yaml.
const STREAM: &str = "kodi";
/// Watchdog cadence, and how many consecutive empty checks count as "nobody listening".
const CHECK_EVERY: Duration = Duration::from_secs(10);
const IDLE_CHECKS: u32 = 3;

/// Sync moves in Kodi's own step size and stays within a second either way.
const SYNC_STEP_MS: i32 = 25;
const SYNC_LIMIT_MS: i32 = 1000;
/// Sign of Kodi's audio offset for "sound earlier". Assumed negative (sound plays earlier
/// relative to the picture); unverified on this setup, so if "Earlier" makes voices come
/// later, flip this to 1.0.
const KODI_SIGN_FOR_EARLIER: f64 = -1.0;

pub fn is_stream_device(device: &str) -> bool {
    device.starts_with(STREAM_DEVICE)
}

#[derive(Debug, thiserror::Error)]
pub enum ListenError {
    #[error("Audio streaming isn't running on the PC")]
    Unavailable,
    #[error("Kodi has no stream output; is the kodi_remote PipeWire sink installed?")]
    NoStreamOutput,
    #[error(transparent)]
    Kodi(#[from] KodiError),
}

/// Shared between requests: go2rtc's API, the output to restore when listening ends, and the
/// sync adjustment (persisted to `sync_path` so it survives restarts).
#[derive(Clone)]
pub struct Listen {
    http: reqwest::Client,
    go2rtc_url: String,
    previous_output: Arc<Mutex<Option<String>>>,
    /// How much earlier than the picture the sound plays while streaming; negative = later.
    sync_earlier_ms: Arc<AtomicI32>,
    sync_path: PathBuf,
}

impl Listen {
    pub fn new(go2rtc_url: String, sync_path: PathBuf) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(5))
            .build()
            .expect("reqwest client builds with static config");
        let saved = std::fs::read_to_string(&sync_path).ok().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
        Self {
            http,
            go2rtc_url: go2rtc_url.trim_end_matches('/').to_owned(),
            previous_output: Default::default(),
            sync_earlier_ms: Arc::new(AtomicI32::new(saved)),
            sync_path,
        }
    }

    pub fn sync_earlier_ms(&self) -> i32 {
        self.sync_earlier_ms.load(Ordering::Relaxed)
    }

    /// Moves the sound `steps` × 25 ms earlier (negative steps: later) and saves the result.
    pub fn nudge_sync(&self, steps: i32) -> i32 {
        let next = (self.sync_earlier_ms() + steps * SYNC_STEP_MS).clamp(-SYNC_LIMIT_MS, SYNC_LIMIT_MS);
        self.sync_earlier_ms.store(next, Ordering::Relaxed);
        if let Some(dir) = self.sync_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(e) = std::fs::write(&self.sync_path, next.to_string()) {
            tracing::warn!("saving sync to {}: {e}", self.sync_path.display());
        }
        next
    }

    /// Kodi's audio offset in seconds for a given "earlier" adjustment.
    fn kodi_offset(earlier_ms: i32) -> f64 {
        KODI_SIGN_FOR_EARLIER * f64::from(earlier_ms) / 1000.0
    }

    /// Applies the sync adjustment to what's playing now, if the sound is being streamed.
    pub async fn apply_sync(&self, kodi: &Kodi) -> Result<(), KodiError> {
        if is_stream_device(&kodi.audio_output().await?) {
            kodi.set_audio_delay(Self::kodi_offset(self.sync_earlier_ms())).await?;
        }
        Ok(())
    }

    /// Runs forever. Kodi keeps audio offsets per video, so once a second this makes sure the
    /// current item has the sync adjustment while streaming and none while on a speaker —
    /// otherwise a Mac-tuned offset would follow a movie back onto the TV.
    pub async fn keep_in_sync(self, kodi: Kodi) {
        let mut applied: Option<(String, i32)> = None;
        loop {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let Ok(Some(item)) = kodi.now_playing_key().await else {
                applied = None;
                continue;
            };
            let Ok(output) = kodi.audio_output().await else { continue };
            let earlier_ms = if is_stream_device(&output) { self.sync_earlier_ms() } else { 0 };
            if applied.as_ref() == Some(&(item.clone(), earlier_ms)) {
                continue;
            }
            match kodi.set_audio_delay(Self::kodi_offset(earlier_ms)).await {
                Ok(()) => applied = Some((item, earlier_ms)),
                Err(e) => tracing::warn!("applying sync: {e}"),
            }
        }
    }

    /// Sends Kodi's sound to the stream sink, remembering the output it replaces.
    pub async fn start(&self, kodi: &Kodi) -> Result<(), ListenError> {
        // Don't silence the TV for a stream nobody can receive.
        self.listeners().await.ok_or(ListenError::Unavailable)?;
        let outputs = kodi.audio_outputs().await?;
        let stream = outputs
            .options
            .iter()
            .filter_map(|o| o.value.as_str())
            .find(|v| is_stream_device(v))
            .ok_or(ListenError::NoStreamOutput)?
            .to_owned();
        if !is_stream_device(&outputs.current) {
            *self.previous_output.lock().expect("lock poisoned") = Some(outputs.current);
        }
        kodi.set_audio_output(&stream).await?;
        Ok(())
    }

    /// Relays a browser's SDP offer to go2rtc and returns its SDP answer.
    pub async fn answer(&self, offer: String) -> Result<String, ListenError> {
        let res = self
            .http
            .post(format!("{}/api/webrtc?src={STREAM}", self.go2rtc_url))
            .body(offer)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status)
            .map_err(|e| {
                tracing::warn!("go2rtc offer: {e}");
                ListenError::Unavailable
            })?;
        res.text().await.map_err(|_| ListenError::Unavailable)
    }

    /// How many browsers are receiving the stream; `None` if go2rtc can't be reached.
    async fn listeners(&self) -> Option<usize> {
        #[derive(Deserialize)]
        struct StreamInfo {
            #[serde(default)]
            consumers: Option<Vec<serde_json::Value>>,
        }
        let info: StreamInfo = self
            .http
            .get(format!("{}/api/streams?src={STREAM}", self.go2rtc_url))
            .send()
            .await
            .ok()?
            .json()
            .await
            .ok()?;
        Some(info.consumers.map_or(0, |c| c.len()))
    }

    /// Runs forever: when Kodi has been playing into the stream sink with nobody listening
    /// (e.g. the remote's tab was closed), switch it back so the TV isn't left silent.
    pub async fn watchdog(self, kodi: Kodi) {
        let mut idle = 0;
        loop {
            tokio::time::sleep(CHECK_EVERY).await;
            let Ok(current) = kodi.audio_output().await else { continue };
            if !is_stream_device(&current) || self.listeners().await.unwrap_or(0) > 0 {
                idle = 0;
                continue;
            }
            idle += 1;
            if idle < IDLE_CHECKS {
                continue;
            }
            idle = 0;
            let Ok(outputs) = kodi.audio_outputs().await else { continue };
            let previous = self.previous_output.lock().expect("lock poisoned").take();
            // Fall back to the TV, then the system default, if we never saw what was replaced.
            let device = |needle: &str| {
                outputs.options.iter().filter_map(|o| o.value.as_str()).find(|v| v.contains(needle)).map(str::to_owned)
            };
            let Some(target) = previous.or_else(|| device("hdmi")).or_else(|| device(":Default")) else { continue };
            match kodi.set_audio_output(&target).await {
                Ok(()) => tracing::info!("nobody listening; sound back on {target}"),
                Err(e) => tracing::warn!("restoring sound output: {e}"),
            }
        }
    }
}
