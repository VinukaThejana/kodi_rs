//! Askama templates and the view models they render, built from Kodi data.

use askama::Template;

use crate::listen;
use crate::kodi::{
    self, AudioOutputs, AudioStream, Episode, Kodi, KodiError, Movie, Playback, Resume, SubtitleStream, TvShow,
    percent_encode,
};

#[derive(Template)]
#[template(path = "index.html")]
pub struct IndexPage {
    pub status: Status,
}

/// The polled header + now-playing card. Re-rendered every couple of seconds.
#[derive(Template)]
#[template(path = "status.html")]
pub struct StatusFragment {
    pub status: Status,
}

#[derive(Template)]
#[template(path = "movies.html")]
pub struct MoviesFragment {
    pub online: bool,
    pub movies: Vec<MovieCard>,
}

#[derive(Template)]
#[template(path = "shows.html")]
pub struct ShowsFragment {
    pub online: bool,
    pub shows: Vec<ShowCard>,
}

/// One show with its episodes, grouped by season.
#[derive(Template)]
#[template(path = "show.html")]
pub struct ShowPage {
    pub title: String,
    pub detail: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub next: Option<NextUp>,
    pub seasons: Vec<Season>,
}

#[derive(Template)]
#[template(path = "audio.html")]
pub struct AudioSheet {
    pub playing: bool,
    pub tracks: Vec<Choice>,
    pub subtitles: Vec<Choice>,
    pub subtitles_off: bool,
    pub outputs: Vec<Choice>,
    /// The stream sink exists, so "Listen on this device" can be offered.
    pub can_listen: bool,
    /// Kodi is playing into the stream sink rather than a speaker.
    pub listening: bool,
    /// The Sync control's current setting, in words.
    pub sync_label: String,
}

pub struct Status {
    pub online: bool,
    /// Kodi is running but its API doesn't answer, so the remote can't control it.
    pub unreachable: bool,
    pub muted: bool,
    pub now: Option<NowPlaying>,
}

pub struct NowPlaying {
    pub title: String,
    pub subtitle: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub elapsed: String,
    pub remaining: String,
    /// Progress through the item, formatted for a CSS percentage.
    pub percent: String,
    /// Elapsed and total seconds, for the system media controls' progress bar.
    pub position: u64,
    pub duration: u64,
    pub paused: bool,
}

pub struct MovieCard {
    pub id: i64,
    pub title: String,
    pub year: String,
    pub poster: Option<String>,
    /// Watched fraction as a CSS percentage, for movies that were started but not finished.
    pub progress: Option<String>,
}

pub struct ShowCard {
    pub id: i64,
    pub title: String,
    pub year: String,
    pub poster: Option<String>,
    pub unwatched: u32,
}

/// The episode the show page's main button plays: in-progress first, then the first unwatched.
pub struct NextUp {
    pub id: i64,
    pub label: String,
    pub toast: String,
}

pub struct Season {
    pub name: String,
    pub episodes: Vec<EpisodeRow>,
}

pub struct EpisodeRow {
    pub id: i64,
    pub number: i64,
    pub title: String,
    pub meta: String,
    pub thumb: Option<String>,
    pub watched: bool,
    pub progress: Option<String>,
    /// Shown in the "Playing …" toast.
    pub toast: String,
}

/// One selectable row in the audio sheet; `value` is what gets posted back.
pub struct Choice {
    pub value: String,
    pub title: String,
    pub detail: String,
    pub selected: bool,
}

impl Status {
    /// Asks Kodi what it's doing. Any failure other than "offline" is shown as idle.
    pub async fn load(kodi: &Kodi) -> Self {
        let playback = match kodi.playback().await {
            Ok(p) => p,
            Err(KodiError::Offline) => {
                let unreachable = kodi::process_running().await;
                return Self { online: false, unreachable, muted: false, now: None };
            }
            Err(e) => {
                tracing::warn!("reading playback: {e}");
                None
            }
        };
        let muted = kodi.volume().await.map(|v| v.muted).unwrap_or(false);
        Self { online: true, unreachable: false, muted, now: playback.map(NowPlaying::from) }
    }

    /// Value for the `data-state` attribute that CSS keys off.
    pub fn state(&self) -> &'static str {
        match (&self.now, self.online) {
            (_, false) if self.unreachable => "unreachable",
            (_, false) => "off",
            (None, true) => "idle",
            (Some(n), true) if n.paused => "paused",
            (Some(_), true) => "playing",
        }
    }
}

impl From<Playback> for NowPlaying {
    fn from(Playback { item, props }: Playback) -> Self {
        let elapsed = props.time.as_secs();
        let total = props.totaltime.as_secs();
        let subtitle = match (item.kind.as_str(), &item.showtitle, item.season, item.episode) {
            ("episode", Some(show), Some(s), Some(e)) => format!("{show} · S{s} E{e}"),
            _ => item.year.filter(|y| *y > 0).map(|y| y.to_string()).unwrap_or_default(),
        };
        let art = |keys: &[&str]| keys.iter().find_map(|k| item.art.get(*k)).map(|src| art_url(src));
        Self {
            poster: art(&["poster", "tvshow.poster", "thumb"]),
            backdrop: art(&["fanart", "tvshow.fanart"]),
            title: if item.title.is_empty() { item.label.clone() } else { item.title.clone() },
            subtitle,
            elapsed: clock(elapsed),
            remaining: format!("-{}", clock(total.saturating_sub(elapsed))),
            percent: format!("{:.2}", props.percentage.clamp(0.0, 100.0)),
            position: elapsed.min(total),
            duration: total,
            paused: props.speed == 0,
        }
    }
}

impl From<Movie> for MovieCard {
    fn from(m: Movie) -> Self {
        Self {
            id: m.movieid,
            year: year(m.year),
            poster: m.art.get("poster").or(m.art.get("thumb")).map(|src| art_url(src)),
            progress: resume_percent(m.resume.as_ref()),
            title: m.title,
        }
    }
}

impl From<TvShow> for ShowCard {
    fn from(s: TvShow) -> Self {
        Self {
            id: s.tvshowid,
            year: year(s.year),
            poster: s.art.get("poster").map(|src| art_url(src)),
            unwatched: s.episode.saturating_sub(s.watchedepisodes),
            title: s.title,
        }
    }
}

impl ShowPage {
    pub fn new(show: TvShow, episodes: Vec<Episode>) -> Self {
        let seasons_label = plural(show.season, "Season");
        let episodes_label = plural(show.episode, "Episode");
        let detail = [year(show.year), seasons_label, episodes_label]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · ");

        let in_progress = episodes.iter().find(|e| resume_percent(e.resume.as_ref()).is_some());
        let next = match in_progress {
            Some(e) => Some((e, "Resume")),
            None => match episodes.iter().find(|e| e.playcount == 0) {
                Some(e) => Some((e, "Play")),
                None => episodes.first().map(|e| (e, "Watch Again")),
            },
        };
        let next = next.map(|(e, verb)| NextUp {
            id: e.episodeid,
            label: format!("{verb} S{} · E{}", e.season, e.episode),
            toast: format!("{} · S{}E{}", show.title, e.season, e.episode),
        });

        // Episodes arrive sorted; split them into seasons, with Specials (season 0) last.
        let mut seasons: Vec<Season> = Vec::new();
        let mut specials: Vec<EpisodeRow> = Vec::new();
        let mut current = None;
        for e in episodes {
            let season = e.season;
            let row = EpisodeRow::new(&show.title, e);
            if season == 0 {
                specials.push(row);
            } else if current == Some(season) {
                seasons.last_mut().expect("season pushed below").episodes.push(row);
            } else {
                current = Some(season);
                seasons.push(Season { name: format!("Season {season}"), episodes: vec![row] });
            }
        }
        if !specials.is_empty() {
            seasons.push(Season { name: "Specials".into(), episodes: specials });
        }

        Self {
            poster: show.art.get("poster").map(|src| art_url(src)),
            backdrop: show.art.get("fanart").or(show.art.get("landscape")).map(|src| art_url(src)),
            title: show.title,
            detail,
            next,
            seasons,
        }
    }
}

impl EpisodeRow {
    fn new(show: &str, e: Episode) -> Self {
        let progress = resume_percent(e.resume.as_ref());
        let runtime = (e.runtime >= 60).then(|| format!("{} min", e.runtime / 60));
        let state = if progress.is_some() {
            Some("In progress")
        } else if e.playcount > 0 {
            Some("Watched")
        } else {
            None
        };
        let meta = runtime.into_iter().chain(state.map(str::to_owned)).collect::<Vec<_>>().join(" · ");
        Self {
            id: e.episodeid,
            number: e.episode,
            meta,
            thumb: e.art.get("thumb").map(|src| art_url(src)),
            watched: e.playcount > 0,
            progress,
            toast: format!("{show} · S{}E{}", e.season, e.episode),
            title: e.title,
        }
    }
}

/// Watched fraction as a CSS percentage, for items that were started but not finished.
fn resume_percent(resume: Option<&Resume>) -> Option<String> {
    resume
        .filter(|r| r.position > 0.0 && r.total > 0.0)
        .map(|r| format!("{:.1}", (r.position / r.total * 100.0).min(100.0)))
}

fn year(y: Option<u32>) -> String {
    y.filter(|y| *y > 0).map(|y| y.to_string()).unwrap_or_default()
}

/// "1 Season", "3 Seasons"; empty for zero.
fn plural(n: u32, noun: &str) -> String {
    match n {
        0 => String::new(),
        1 => format!("1 {noun}"),
        n => format!("{n} {noun}s"),
    }
}

impl AudioSheet {
    /// `sync_earlier_ms` is the "Listen on this device" sync adjustment, shown while listening.
    pub async fn load(kodi: &Kodi, sync_earlier_ms: i32) -> Result<Self, KodiError> {
        let playback = kodi.playback().await?;
        let audio_outputs = kodi.audio_outputs().await?;
        let can_listen =
            audio_outputs.options.iter().any(|o| o.value.as_str().is_some_and(listen::is_stream_device));
        let listening = listen::is_stream_device(&audio_outputs.current);
        let mut sheet = Self {
            playing: false,
            tracks: vec![],
            subtitles: vec![],
            subtitles_off: true,
            outputs: output_choices(audio_outputs),
            can_listen,
            listening,
            sync_label: match sync_earlier_ms {
                0 => "No adjustment".to_owned(),
                ms if ms > 0 => format!("Sound {ms} ms earlier"),
                ms => format!("Sound {} ms later", -ms),
            },
        };
        if let Some(Playback { props, .. }) = playback {
            let current_audio = props.currentaudiostream.and_then(|s| s.index);
            let current_sub = props.currentsubtitle.and_then(|s| s.index).filter(|_| props.subtitleenabled);
            sheet.playing = true;
            sheet.tracks = props.audiostreams.iter().map(|s| audio_choice(s, current_audio)).collect();
            sheet.subtitles = props.subtitles.iter().map(|s| subtitle_choice(s, current_sub)).collect();
            sheet.subtitles_off = current_sub.is_none();
        }
        Ok(sheet)
    }
}

fn audio_choice(s: &AudioStream, current: Option<i64>) -> Choice {
    let channels = match s.channels {
        0 => None,
        1 => Some("Mono".to_owned()),
        2 => Some("Stereo".to_owned()),
        n => Some(format!("{}.1", n - 1)),
    };
    let detail = [Some(s.codec.to_uppercase()).filter(|c| !c.is_empty()), channels]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
    Choice {
        value: s.index.to_string(),
        title: stream_title(&s.name, &s.language, s.index),
        detail,
        selected: current == Some(s.index),
    }
}

fn subtitle_choice(s: &SubtitleStream, current: Option<i64>) -> Choice {
    Choice {
        value: s.index.to_string(),
        title: stream_title(&s.name, &s.language, s.index),
        detail: if s.name.is_empty() { String::new() } else { language_name(&s.language).to_owned() },
        selected: current == Some(s.index),
    }
}

fn stream_title(name: &str, language: &str, index: i64) -> String {
    match (name.trim(), language_name(language)) {
        ("", "") => format!("Track {}", index + 1),
        ("", lang) => lang.to_owned(),
        (name, _) => name.to_owned(),
    }
}

/// Friendly names for the ISO 639-2 codes Kodi reports; unknown codes pass through.
fn language_name(code: &str) -> &str {
    match code {
        "eng" => "English",
        "sin" => "Sinhala",
        "tam" => "Tamil",
        "hin" => "Hindi",
        "jpn" => "Japanese",
        "kor" => "Korean",
        "chi" | "zho" => "Chinese",
        "spa" => "Spanish",
        "fre" | "fra" => "French",
        "ger" | "deu" => "German",
        "ita" => "Italian",
        "por" => "Portuguese",
        "rus" => "Russian",
        "ara" => "Arabic",
        "und" => "",
        other => other,
    }
}

/// Kodi labels devices like "HDMI Controller … [SmartTV], HDMI Controller … (PIPEWIRE)".
/// Turn those into a short name plus the raw hardware name as detail.
fn output_choices(outputs: AudioOutputs) -> Vec<Choice> {
    outputs
        .options
        .into_iter()
        .filter_map(|o| {
            let value = o.value.as_str()?.to_owned();
            if listen::is_stream_device(&value) {
                return None; // offered as "This device" instead
            }
            let hardware = o.label.split(", ").next().unwrap_or(&o.label).to_owned();
            // Device ids look like "PIPEWIRE:Default|Default Output Device (PIPEWIRE)".
            let device = value.split('|').next().unwrap_or_default();
            let title = if device.ends_with(":Default") {
                "System Default".to_owned()
            } else if hardware.contains("HDMI") {
                bracketed(&hardware).map_or("TV · HDMI".to_owned(), |name| format!("{name} · HDMI"))
            } else if hardware.contains("Analog") {
                "Analog Out".to_owned()
            } else {
                hardware.clone()
            };
            let detail = if device.contains("raop_sink") {
                // PipeWire's AirPlay outputs (pipewire-zeroconf), e.g. the MacBook.
                "AirPlay".to_owned()
            } else if title == hardware {
                String::new()
            } else {
                hardware
            };
            Some(Choice { selected: value == outputs.current, value, title, detail })
        })
        .collect()
}

fn bracketed(s: &str) -> Option<&str> {
    let start = s.rfind('[')? + 1;
    let end = start + s[start..].find(']')?;
    Some(&s[start..end])
}

fn art_url(src: &str) -> String {
    format!("art?src={}", percent_encode(src))
}

/// Formats seconds as `m:ss`, or `h:mm:ss` past an hour.
fn clock(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}
