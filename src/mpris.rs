//! What the browser is playing, read over MPRIS (the desktop media-player D-Bus interface,
//! `org.mpris.MediaPlayer2.Player`) on the session bus. Chrome publishes a page's media session
//! there as `org.mpris.MediaPlayer2.chromium.instance<pid>`: title, artwork, length, position,
//! and play/pause and seek controls. The status card shows it like Kodi's now playing.

use zbus::{Connection, DeserializeDict, OwnedObjectPath, Type};

const PATH: &str = "/org/mpris/MediaPlayer2";
const PLAYER: &str = "org.mpris.MediaPlayer2.Player";
const CHROME: &str = "org.mpris.MediaPlayer2.chromium.";
/// MPRIS times are microseconds.
const MICROS: f64 = 1_000_000.0;

#[derive(Debug, thiserror::Error)]
pub enum MprisError {
    #[error("no session D-Bus, so the browser's playback can't be controlled")]
    Unavailable,
    #[error("the browser isn't playing anything")]
    NoPlayer,
    #[error(transparent)]
    Bus(#[from] zbus::Error),
}

pub type Result<T> = std::result::Result<T, MprisError>;

/// The `org.mpris.MediaPlayer2.Player` properties this module reads.
#[derive(Debug, Default, DeserializeDict, Type)]
#[zbus(signature = "a{sv}", rename_all = "PascalCase")]
struct PlayerProps {
    playback_status: Option<String>,
    metadata: Option<Metadata>,
    position: Option<i64>,
}

#[derive(Debug, Default, DeserializeDict, Type)]
#[zbus(signature = "a{sv}")]
struct Metadata {
    #[zbus(rename = "xesam:title")]
    title: Option<String>,
    #[zbus(rename = "xesam:artist")]
    artist: Option<Vec<String>>,
    #[zbus(rename = "xesam:album")]
    album: Option<String>,
    #[zbus(rename = "mpris:artUrl")]
    art_url: Option<String>,
    #[zbus(rename = "mpris:length")]
    length: Option<i64>,
    #[zbus(rename = "mpris:trackid")]
    track_id: Option<OwnedObjectPath>,
}

/// The browser's media, while it has some loaded (playing or paused).
#[derive(Debug)]
pub struct Playing {
    pub title: String,
    /// The artist, else the album: what the page gives below the title.
    pub subtitle: String,
    /// The artwork as a local file Chrome wrote, if any (see [`Mpris::art_file`]).
    pub art_file: Option<String>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
}

#[derive(Clone)]
pub struct Mpris {
    bus: Option<Connection>,
}

impl Mpris {
    pub async fn new() -> Self {
        let bus = Connection::session()
            .await
            .inspect_err(|e| tracing::warn!("no session D-Bus, so no browser now-playing: {e}"))
            .ok();
        Self { bus }
    }

    fn bus(&self) -> Result<&Connection> {
        self.bus.as_ref().ok_or(MprisError::Unavailable)
    }

    /// Chrome's player name on the bus, if Chrome is running.
    async fn player(&self) -> Result<Option<String>> {
        let reply =
            self.bus()?.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"), "ListNames", &()).await?;
        let names: Vec<String> = reply.body().deserialize()?;
        Ok(names.into_iter().find(|n| n.starts_with(CHROME)))
    }

    async fn require_player(&self) -> Result<String> {
        self.player().await?.ok_or(MprisError::NoPlayer)
    }

    async fn props(&self, player: &str) -> Result<PlayerProps> {
        let reply =
            self.bus()?.call_method(Some(player), PATH, Some("org.freedesktop.DBus.Properties"), "GetAll", &(PLAYER,)).await?;
        Ok(reply.body().deserialize()?)
    }

    /// What the browser is playing; `None` when nothing is loaded (MPRIS "Stopped", or no
    /// length yet, as on a page that hasn't started a video).
    pub async fn playing(&self) -> Result<Option<Playing>> {
        let Some(player) = self.player().await? else { return Ok(None) };
        let props = self.props(&player).await?;
        let status = props.playback_status.unwrap_or_default();
        let meta = props.metadata.unwrap_or_default();
        let duration = meta.length.unwrap_or(0) as f64 / MICROS;
        if status == "Stopped" || duration <= 0.0 {
            return Ok(None);
        }
        let artist = meta.artist.map(|a| a.join(", ")).filter(|a| !a.is_empty());
        Ok(Some(Playing {
            title: meta.title.filter(|t| !t.is_empty()).unwrap_or_else(|| "Apple TV".to_owned()),
            subtitle: artist.or(meta.album).unwrap_or_default(),
            art_file: meta.art_url.as_deref().and_then(art_file).map(str::to_owned),
            position: (props.position.unwrap_or(0) as f64 / MICROS).clamp(0.0, duration),
            duration,
            paused: status != "Playing",
        }))
    }

    /// The artwork file Chrome is showing now, for the `/browser/art` route.
    pub async fn art_file(&self) -> Result<Option<String>> {
        Ok(self.playing().await?.and_then(|p| p.art_file))
    }

    pub async fn play_pause(&self) -> Result<()> {
        self.call("PlayPause", &()).await
    }

    pub async fn stop(&self) -> Result<()> {
        self.call("Stop", &()).await
    }

    /// Jumps `seconds` forward (or back, if negative).
    pub async fn skip(&self, seconds: i64) -> Result<()> {
        self.call("Seek", &(seconds * 1_000_000,)).await
    }

    /// Jumps to `percent` of the way through. MPRIS's SetPosition needs the track id, which
    /// Chrome doesn't always give; then it's a relative Seek from the current position.
    pub async fn seek_to(&self, percent: f64) -> Result<()> {
        let player = self.require_player().await?;
        let props = self.props(&player).await?;
        let meta = props.metadata.unwrap_or_default();
        let target = (meta.length.unwrap_or(0) as f64 * percent / 100.0) as i64;
        match meta.track_id {
            Some(track) => self.call("SetPosition", &(track, target)).await,
            None => self.call("Seek", &(target - props.position.unwrap_or(0),)).await,
        }
    }

    async fn call<B>(&self, method: &'static str, body: &B) -> Result<()>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        let player = self.require_player().await?;
        self.bus()?.call_method(Some(player.as_str()), PATH, Some(PLAYER), method, body).await?;
        Ok(())
    }
}

/// The local path in a `file://` artwork URL, if it's one Chrome writes (its temporary
/// `/tmp/.com.google.Chrome.*` files). Nothing else is served from disk.
fn art_file(url: &str) -> Option<&str> {
    let path = url.strip_prefix("file://")?;
    let name = path.strip_prefix("/tmp/")?;
    (name.starts_with(".com.google.Chrome.") && !name.contains('/')).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_only_chromes_artwork_files() {
        assert_eq!(art_file("file:///tmp/.com.google.Chrome.6CbnSt"), Some("/tmp/.com.google.Chrome.6CbnSt"));
        assert_eq!(art_file("file:///tmp/.com.google.Chrome.x/../../etc/passwd"), None);
        assert_eq!(art_file("file:///home/vinuka/.ssh/id_ed25519"), None);
        assert_eq!(art_file("https://is1-ssl.mzstatic.com/image.jpg"), None);
    }
}
