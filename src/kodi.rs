//! Typed client for Kodi's JSON-RPC API (`/jsonrpc`) and image endpoint (`/image/`).

use std::{collections::HashMap, time::Duration};

use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};

#[derive(Debug, thiserror::Error)]
pub enum KodiError {
    /// Kodi's API isn't answering: it isn't running, or it's running without its web server.
    #[error("Kodi is off")]
    Offline,
    #[error("Kodi error {code} from {method}: {message}")]
    Rpc { method: &'static str, code: i64, message: String },
    #[error("request to Kodi failed: {0}")]
    Http(#[from] reqwest::Error),
}

pub type Result<T> = std::result::Result<T, KodiError>;

/// Connection details for one Kodi instance. Cheap to clone.
#[derive(Clone)]
pub struct Kodi {
    http: reqwest::Client,
    base_url: String,
    user: String,
    password: String,
}

#[derive(Deserialize)]
struct RpcResponse<R> {
    result: Option<R>,
    error: Option<RpcErrorBody>,
}

#[derive(Deserialize)]
struct RpcErrorBody {
    code: i64,
    message: String,
}

/// Directional and menu keys on the remote, mapped to Kodi's `Input.*` methods.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NavKey {
    Up,
    Down,
    Left,
    Right,
    Select,
    Back,
    Home,
}

impl NavKey {
    fn method(self) -> &'static str {
        match self {
            Self::Up => "Input.Up",
            Self::Down => "Input.Down",
            Self::Left => "Input.Left",
            Self::Right => "Input.Right",
            Self::Select => "Input.Select",
            Self::Back => "Input.Back",
            Self::Home => "Input.Home",
        }
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VolumeStep {
    Up,
    Down,
}

/// Which subtitle track to show; `Off` hides subtitles. Parses from `"off"` or a track index.
#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(try_from = "String")]
pub enum Subtitle {
    Off,
    Track(i64),
}

impl TryFrom<String> for Subtitle {
    type Error = String;

    fn try_from(s: String) -> std::result::Result<Self, Self::Error> {
        match s.as_str() {
            "off" => Ok(Self::Off),
            n => n.parse().map(Self::Track).map_err(|_| format!("invalid subtitle: {s}")),
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct Volume {
    pub volume: u8,
    pub muted: bool,
}

#[derive(Debug, Deserialize)]
struct ActivePlayer {
    playerid: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
pub struct Item {
    pub label: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub year: Option<u32>,
    #[serde(default)]
    pub showtitle: Option<String>,
    #[serde(default)]
    pub season: Option<i64>,
    #[serde(default)]
    pub episode: Option<i64>,
    #[serde(default)]
    pub art: HashMap<String, String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct KodiTime {
    hours: u64,
    minutes: u64,
    seconds: u64,
}

impl KodiTime {
    pub fn as_secs(&self) -> u64 {
        self.hours * 3600 + self.minutes * 60 + self.seconds
    }
}

#[derive(Debug, Deserialize)]
pub struct AudioStream {
    pub index: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub codec: String,
    #[serde(default)]
    pub channels: u32,
}

#[derive(Debug, Deserialize)]
pub struct SubtitleStream {
    pub index: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub language: String,
}

/// Kodi reports "no stream selected" as an empty object, so the index is optional.
#[derive(Debug, Default, Deserialize)]
pub struct CurrentStream {
    pub index: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct PlayerProps {
    pub time: KodiTime,
    pub totaltime: KodiTime,
    pub percentage: f64,
    pub speed: i64,
    #[serde(default)]
    pub audiostreams: Vec<AudioStream>,
    #[serde(default)]
    pub currentaudiostream: Option<CurrentStream>,
    #[serde(default)]
    pub subtitles: Vec<SubtitleStream>,
    #[serde(default)]
    pub currentsubtitle: Option<CurrentStream>,
    #[serde(default)]
    pub subtitleenabled: bool,
}

/// What's on screen right now, if anything.
#[derive(Debug)]
pub struct Playback {
    pub item: Item,
    pub props: PlayerProps,
}

#[derive(Debug, Deserialize)]
pub struct Movie {
    pub movieid: i64,
    pub title: String,
    #[serde(default)]
    pub year: Option<u32>,
    #[serde(default)]
    pub art: HashMap<String, String>,
    #[serde(default)]
    pub resume: Option<Resume>,
}

#[derive(Debug, Deserialize)]
pub struct TvShow {
    pub tvshowid: i64,
    pub title: String,
    #[serde(default)]
    pub year: Option<u32>,
    #[serde(default)]
    pub art: HashMap<String, String>,
    /// Number of seasons.
    #[serde(default)]
    pub season: u32,
    /// Number of episodes.
    #[serde(default)]
    pub episode: u32,
    #[serde(default)]
    pub watchedepisodes: u32,
}

#[derive(Debug, Deserialize)]
pub struct Episode {
    pub episodeid: i64,
    pub title: String,
    pub season: i64,
    pub episode: i64,
    #[serde(default)]
    pub playcount: u32,
    /// Seconds; 0 when the scraper didn't provide one.
    #[serde(default)]
    pub runtime: u64,
    #[serde(default)]
    pub art: HashMap<String, String>,
    #[serde(default)]
    pub resume: Option<Resume>,
}

const SHOW_PROPERTIES: [&str; 6] = ["title", "year", "art", "season", "episode", "watchedepisodes"];

#[derive(Debug, Deserialize)]
pub struct Resume {
    pub position: f64,
    pub total: f64,
}

#[derive(Debug, Deserialize)]
pub struct SettingOption {
    pub label: String,
    pub value: Value,
}

#[derive(Debug, Deserialize)]
struct Setting {
    id: String,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    options: Vec<SettingOption>,
}

/// Kodi's audio output setting: the selected device id and every device it can use.
#[derive(Debug)]
pub struct AudioOutputs {
    pub current: String,
    pub options: Vec<SettingOption>,
}

const AUDIO_DEVICE_SETTING: &str = "audiooutput.audiodevice";

impl Kodi {
    pub fn new(base_url: String, user: String, password: String) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_millis(500))
            .timeout(Duration::from_secs(4))
            .build()
            .expect("reqwest client builds with static config");
        Self { http, base_url: base_url.trim_end_matches('/').to_owned(), user, password }
    }

    /// Calls one JSON-RPC method and decodes its `result`.
    async fn call<R: DeserializeOwned>(&self, method: &'static str, params: Value) -> Result<R> {
        let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
        let res = self
            .http
            .post(format!("{}/jsonrpc", self.base_url))
            .basic_auth(&self.user, Some(&self.password))
            .json(&body)
            .send()
            .await
            .map_err(offline_if_unreachable)?
            .error_for_status()?
            .json::<RpcResponse<R>>()
            .await?;
        match (res.result, res.error) {
            (Some(result), _) => Ok(result),
            (None, Some(e)) => Err(KodiError::Rpc { method, code: e.code, message: e.message }),
            (None, None) => Err(KodiError::Rpc { method, code: 0, message: "empty response".into() }),
        }
    }

    /// Like [`Kodi::call`] for methods whose result we don't need (usually `"OK"`).
    async fn send(&self, method: &'static str, params: Value) -> Result<()> {
        self.call::<Value>(method, params).await.map(drop)
    }

    pub async fn is_online(&self) -> bool {
        self.call::<Value>("JSONRPC.Ping", json!({})).await.is_ok()
    }

    /// The video player if one is active, otherwise any active player.
    async fn active_player(&self) -> Result<Option<i64>> {
        let players: Vec<ActivePlayer> = self.call("Player.GetActivePlayers", json!({})).await?;
        let video = players.iter().find(|p| p.kind == "video");
        Ok(video.or(players.first()).map(|p| p.playerid))
    }

    async fn require_player(&self) -> Result<i64> {
        self.active_player()
            .await?
            .ok_or_else(|| KodiError::Rpc { method: "Player.GetActivePlayers", code: 0, message: "Nothing is playing".into() })
    }

    pub async fn playback(&self) -> Result<Option<Playback>> {
        let Some(player_id) = self.active_player().await? else { return Ok(None) };
        #[derive(Deserialize)]
        struct GetItem {
            item: Item,
        }
        let GetItem { item } = self
            .call(
                "Player.GetItem",
                json!({ "playerid": player_id, "properties": ["title", "year", "art", "showtitle", "season", "episode"] }),
            )
            .await?;
        let props = self
            .call(
                "Player.GetProperties",
                json!({ "playerid": player_id, "properties": [
                    "time", "totaltime", "percentage", "speed",
                    "audiostreams", "currentaudiostream", "subtitles", "currentsubtitle", "subtitleenabled"
                ] }),
            )
            .await?;
        Ok(Some(Playback { item, props }))
    }

    pub async fn volume(&self) -> Result<Volume> {
        self.call("Application.GetProperties", json!({ "properties": ["volume", "muted"] })).await
    }

    pub async fn nav(&self, key: NavKey) -> Result<()> {
        self.send(key.method(), json!({})).await
    }

    pub async fn play_pause(&self) -> Result<()> {
        let id = self.require_player().await?;
        self.send("Player.PlayPause", json!({ "playerid": id })).await
    }

    pub async fn stop(&self) -> Result<()> {
        let id = self.require_player().await?;
        self.send("Player.Stop", json!({ "playerid": id })).await
    }

    /// Jumps relative to the current position; negative seconds go back.
    pub async fn skip(&self, seconds: i64) -> Result<()> {
        let id = self.require_player().await?;
        self.send("Player.Seek", json!({ "playerid": id, "value": { "seconds": seconds } })).await
    }

    pub async fn seek_to(&self, percentage: f64) -> Result<()> {
        let id = self.require_player().await?;
        self.send("Player.Seek", json!({ "playerid": id, "value": { "percentage": percentage } })).await
    }

    pub async fn step_volume(&self, step: VolumeStep) -> Result<Volume> {
        let dir = match step {
            VolumeStep::Up => "increment",
            VolumeStep::Down => "decrement",
        };
        self.send("Application.SetVolume", json!({ "volume": dir })).await?;
        self.volume().await
    }

    pub async fn toggle_mute(&self) -> Result<Volume> {
        self.send("Application.SetMute", json!({ "mute": "toggle" })).await?;
        self.volume().await
    }

    pub async fn movies(&self) -> Result<Vec<Movie>> {
        #[derive(Deserialize)]
        struct GetMovies {
            #[serde(default)]
            movies: Vec<Movie>,
        }
        let res: GetMovies = self
            .call(
                "VideoLibrary.GetMovies",
                json!({
                    "properties": ["title", "year", "art", "resume"],
                    "sort": { "method": "title", "ignorearticle": true },
                }),
            )
            .await?;
        Ok(res.movies)
    }

    /// Starts a library movie, resuming where it was left off.
    pub async fn play_movie(&self, movie_id: i64) -> Result<()> {
        self.send("Player.Open", json!({ "item": { "movieid": movie_id }, "options": { "resume": true } }))
            .await
    }

    pub async fn tv_shows(&self) -> Result<Vec<TvShow>> {
        #[derive(Deserialize)]
        struct GetTvShows {
            #[serde(default)]
            tvshows: Vec<TvShow>,
        }
        let res: GetTvShows = self
            .call(
                "VideoLibrary.GetTVShows",
                json!({ "properties": SHOW_PROPERTIES, "sort": { "method": "title", "ignorearticle": true } }),
            )
            .await?;
        Ok(res.tvshows)
    }

    pub async fn tv_show(&self, tvshow_id: i64) -> Result<TvShow> {
        #[derive(Deserialize)]
        struct GetTvShowDetails {
            tvshowdetails: TvShow,
        }
        let res: GetTvShowDetails = self
            .call("VideoLibrary.GetTVShowDetails", json!({ "tvshowid": tvshow_id, "properties": SHOW_PROPERTIES }))
            .await?;
        Ok(res.tvshowdetails)
    }

    /// Every episode of a show, ordered by season then episode.
    pub async fn episodes(&self, tvshow_id: i64) -> Result<Vec<Episode>> {
        #[derive(Deserialize)]
        struct GetEpisodes {
            #[serde(default)]
            episodes: Vec<Episode>,
        }
        let mut res: GetEpisodes = self
            .call(
                "VideoLibrary.GetEpisodes",
                json!({
                    "tvshowid": tvshow_id,
                    "properties": ["title", "season", "episode", "playcount", "runtime", "art", "resume"],
                }),
            )
            .await?;
        res.episodes.sort_by_key(|e| (e.season, e.episode));
        Ok(res.episodes)
    }

    /// Starts a library episode, resuming where it was left off.
    pub async fn play_episode(&self, episode_id: i64) -> Result<()> {
        self.send("Player.Open", json!({ "item": { "episodeid": episode_id }, "options": { "resume": true } }))
            .await
    }

    pub async fn set_audio_stream(&self, index: i64) -> Result<()> {
        let id = self.require_player().await?;
        self.send("Player.SetAudioStream", json!({ "playerid": id, "stream": index })).await
    }

    pub async fn set_subtitle(&self, subtitle: Subtitle) -> Result<()> {
        let id = self.require_player().await?;
        let params = match subtitle {
            Subtitle::Off => json!({ "playerid": id, "subtitle": "off" }),
            Subtitle::Track(index) => json!({ "playerid": id, "subtitle": index, "enable": true }),
        };
        self.send("Player.SetSubtitle", params).await
    }

    pub async fn audio_outputs(&self) -> Result<AudioOutputs> {
        #[derive(Deserialize)]
        struct GetSettings {
            settings: Vec<Setting>,
        }
        let res: GetSettings = self
            .call(
                "Settings.GetSettings",
                json!({ "level": "expert", "filter": { "section": "system", "category": "audio" } }),
            )
            .await?;
        let setting = res
            .settings
            .into_iter()
            .find(|s| s.id == AUDIO_DEVICE_SETTING)
            .ok_or_else(|| KodiError::Rpc { method: "Settings.GetSettings", code: 0, message: "audio device setting missing".into() })?;
        Ok(AudioOutputs {
            current: setting.value.as_str().unwrap_or_default().to_owned(),
            options: setting.options,
        })
    }

    /// Shifts the current item's sound against its picture, in seconds (multiples of 0.025).
    pub async fn set_audio_delay(&self, seconds: f64) -> Result<()> {
        let id = self.require_player().await?;
        self.send("Player.SetAudioDelay", json!({ "playerid": id, "offset": seconds })).await
    }

    /// Identifies what's playing (player, library id and file), or `None` when nothing is,
    /// so callers can tell when a new item starts.
    pub async fn now_playing_key(&self) -> Result<Option<String>> {
        let Some(player_id) = self.active_player().await? else { return Ok(None) };
        let item: Value =
            self.call("Player.GetItem", json!({ "playerid": player_id, "properties": ["file"] })).await?;
        let item = &item["item"];
        Ok(Some(format!("{player_id}:{}:{}:{}", item["type"], item["id"], item["file"])))
    }

    /// The selected audio device id alone; cheaper than [`Kodi::audio_outputs`].
    pub async fn audio_output(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct GetSettingValue {
            value: Value,
        }
        let res: GetSettingValue =
            self.call("Settings.GetSettingValue", json!({ "setting": AUDIO_DEVICE_SETTING })).await?;
        Ok(res.value.as_str().unwrap_or_default().to_owned())
    }

    pub async fn set_audio_output(&self, device: &str) -> Result<()> {
        self.send("Settings.SetSettingValue", json!({ "setting": AUDIO_DEVICE_SETTING, "value": device }))
            .await
    }

    pub async fn quit(&self) -> Result<()> {
        self.send("Application.Quit", json!({})).await
    }

    /// Fetches artwork for an `image://…` URL as `(content-type, bytes)`.
    pub async fn image(&self, src: &str) -> Result<(Option<String>, Vec<u8>)> {
        let res = self
            .http
            .get(format!("{}/image/{}", self.base_url, percent_encode(src)))
            .basic_auth(&self.user, Some(&self.password))
            .send()
            .await
            .map_err(offline_if_unreachable)?
            .error_for_status()?;
        let content_type = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.is_empty())
            .map(str::to_owned);
        Ok((content_type, res.bytes().await?.to_vec()))
    }
}

fn offline_if_unreachable(e: reqwest::Error) -> KodiError {
    if e.is_connect() || e.is_timeout() { KodiError::Offline } else { KodiError::Http(e) }
}

/// Whether a Kodi process exists on this machine, whoever started it. Paired with an
/// unreachable API this means Kodi is up without its web server (e.g. port 8080 was taken).
pub async fn process_running() -> bool {
    tokio::process::Command::new("pgrep")
        .args(["-x", "kodi.bin"])
        .output()
        .await
        .is_ok_and(|out| out.status.success())
}

/// Encodes every byte outside RFC 3986's unreserved set, as Kodi's `/image/` route expects.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
