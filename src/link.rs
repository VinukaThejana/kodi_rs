//! Links sent to the remote — pasted into the search bar, or shared from the iPhone through
//! the "Play on Kodi" shortcut — turned into something Kodi can play.

use url::Url;

/// A link Kodi can play. YouTube goes through the YouTube add-on; anything else has to point
/// straight at a media file or stream, which Kodi opens itself.
#[derive(Debug, PartialEq, Eq)]
pub enum Link {
    YouTubeVideo { id: String, start: Option<u32> },
    YouTubePlaylist(String),
    Media(Url),
}

/// How Kodi starts a [`Link`].
#[derive(Debug, PartialEq, Eq)]
pub enum Start {
    /// `Player.Open` this path; Kodi answers once the stream has started.
    Open(String),
    /// Run the add-on with these params, as Kodi's `RunAddon` does. A YouTube playlist needs
    /// this: the add-on queues every video itself, and `Player.Open` would replace that queue
    /// with just the first one.
    RunAddon { addon: &'static str, params: String },
}

const YOUTUBE: &str = "plugin.video.youtube";

/// Extensions Kodi can open directly over HTTP.
const MEDIA: &[&str] = &["mp4", "m4v", "mkv", "webm", "mov", "avi", "ts", "m3u8", "mpd", "mp3", "m4a", "flac"];

impl Link {
    /// Reads the first `http(s)://` URL in `text`. Share sheets often wrap the link in a title
    /// ("Watch this! https://youtu.be/…"), so the URL doesn't have to stand alone.
    pub fn find(text: &str) -> Option<Self> {
        let url = text.split_whitespace().find_map(|word| {
            let start = word.find("http://").or_else(|| word.find("https://"))?;
            Url::parse(&word[start..]).ok()
        })?;
        youtube(&url).or_else(|| media(url))
    }

    /// How Kodi should start this link.
    pub fn start(&self) -> Start {
        match self {
            Self::YouTubeVideo { id, start: None } => Start::Open(format!("plugin://{YOUTUBE}/play/?video_id={id}")),
            Self::YouTubeVideo { id, start: Some(s) } => {
                Start::Open(format!("plugin://{YOUTUBE}/play/?video_id={id}&seek={s}"))
            }
            // Without an order the add-on asks for one in a dialog on the TV.
            Self::YouTubePlaylist(id) => {
                Start::RunAddon { addon: YOUTUBE, params: format!("/play/?playlist_id={id}&order=default&action=play") }
            }
            Self::Media(url) => Start::Open(url.as_str().to_owned()),
        }
    }

    /// What the toast says is playing.
    pub fn label(&self) -> &'static str {
        match self {
            Self::YouTubeVideo { .. } => "YouTube video",
            Self::YouTubePlaylist(_) => "YouTube playlist",
            Self::Media(_) => "link",
        }
    }
}

/// `youtu.be/ID`, `youtube.com/watch?v=ID`, `/shorts/ID`, `/live/ID`, `/embed/ID` and
/// `/playlist?list=ID`, on any YouTube host (www, m, music, nocookie). A video inside a
/// playlist plays just that video.
fn youtube(url: &Url) -> Option<Link> {
    let host = url.host_str()?;
    let query = |key: &str| url.query_pairs().find(|(k, _)| k == key).map(|(_, v)| v.into_owned());
    let mut segments = url.path_segments()?.filter(|s| !s.is_empty());

    let video = if host == "youtu.be" {
        segments.next().map(str::to_owned)
    } else if ["youtube.com", "youtube-nocookie.com"].iter().any(|d| host == *d || host.ends_with(&format!(".{d}"))) {
        match segments.next() {
            Some("watch") => query("v"),
            Some("shorts" | "live" | "embed" | "v") => segments.next().map(str::to_owned),
            Some("playlist") => {
                return query("list").filter(|id| is_id(id, 64)).map(Link::YouTubePlaylist);
            }
            _ => None,
        }
    } else {
        return None;
    };
    let id = video.filter(|id| id.len() == 11 && is_id(id, 11))?;
    let start = query("t").or_else(|| query("start")).and_then(|t| seconds(&t));
    Some(Link::YouTubeVideo { id, start })
}

fn media(url: Url) -> Option<Link> {
    let ext = url.path().rsplit_once('.')?.1.to_ascii_lowercase();
    let web = matches!(url.scheme(), "http" | "https");
    (web && MEDIA.contains(&ext.as_str())).then_some(Link::Media(url))
}

fn is_id(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// YouTube's `t=` start time: `90`, `90s`, `1m30s` or `1h2m3s`.
fn seconds(t: &str) -> Option<u32> {
    if let Ok(n) = t.parse() {
        return Some(n);
    }
    let (mut total, mut n) = (0u32, 0u32);
    for c in t.chars() {
        match c {
            '0'..='9' => n = n.checked_mul(10)?.checked_add(c.to_digit(10)?)?,
            'h' => (total, n) = (total.saturating_add(n.saturating_mul(3600)), 0),
            'm' => (total, n) = (total.saturating_add(n.saturating_mul(60)), 0),
            's' => (total, n) = (total.saturating_add(n), 0),
            _ => return None,
        }
    }
    (n == 0).then_some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn video(id: &str, start: Option<u32>) -> Option<Link> {
        Some(Link::YouTubeVideo { id: id.to_owned(), start })
    }

    #[test]
    fn reads_youtube_links_in_every_shape() {
        let id = "dQw4w9WgXcQ";
        assert_eq!(Link::find(&format!("https://www.youtube.com/watch?v={id}&list=PL1")), video(id, None));
        assert_eq!(Link::find(&format!("https://youtu.be/{id}?si=abc&t=95")), video(id, Some(95)));
        assert_eq!(Link::find(&format!("https://m.youtube.com/watch?v={id}&t=1m30s")), video(id, Some(90)));
        assert_eq!(Link::find(&format!("https://youtube.com/shorts/{id}?feature=share")), video(id, None));
        assert_eq!(Link::find(&format!("Check this out https://www.youtube.com/live/{id}")), video(id, None));
        assert_eq!(
            Link::find("https://music.youtube.com/playlist?list=PLx0sYbCqOb8TBPRdmBHs5Iftvv9TPboYG"),
            Some(Link::YouTubePlaylist("PLx0sYbCqOb8TBPRdmBHs5Iftvv9TPboYG".into()))
        );
    }

    #[test]
    fn plays_youtube_through_the_addon_and_media_directly() {
        let id = "dQw4w9WgXcQ";
        let open = |path: &str| Start::Open(path.to_owned());
        assert_eq!(
            video(id, Some(90)).unwrap().start(),
            open(&format!("plugin://{YOUTUBE}/play/?video_id={id}&seek=90"))
        );
        let mp4 = Link::find("https://example.com/films/clip.MP4?token=x").unwrap();
        assert_eq!(mp4.start(), open("https://example.com/films/clip.MP4?token=x"));
        let Start::RunAddon { params, .. } = Link::YouTubePlaylist("PL1".into()).start() else { panic!() };
        assert_eq!(params, "/play/?playlist_id=PL1&order=default&action=play");
    }

    #[test]
    fn rejects_what_kodi_cannot_play() {
        assert_eq!(Link::find("breaking bad"), None);
        assert_eq!(Link::find("https://www.youtube.com/@somechannel"), None);
        assert_eq!(Link::find("https://youtu.be/short"), None);
        assert_eq!(Link::find("https://example.com/watch"), None);
        assert_eq!(Link::find("https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ"), None);
    }
}
