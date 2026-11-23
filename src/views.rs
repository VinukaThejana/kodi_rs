//! Askama templates and the view models they render, built from Kodi data.

use askama::Template;

use crate::bluetooth::Headphones;
use crate::browser::{self, Site};
use crate::mpris::{self, Mpris};
use crate::sound;
use crate::listen;
use crate::kodi::{
    self, AudioOutputs, AudioStream, DirEntry, Episode, FileType, Kodi, KodiError, Movie, Pick, Playback, Resume,
    SubtitleStream, TvShow, percent_encode,
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

/// The Home segment: the rows of Kodi's home screen, each loading on its own (see [`RowFragment`]).
/// Its `data-addon-url` starts the page trail, so Back from a show opened here returns here.
#[derive(Template)]
#[template(path = "home.html")]
pub struct HomePage {
    pub online: bool,
    pub rows: &'static [Row],
}

/// One loaded Home row; it renders nothing when the row is empty, which removes it.
#[derive(Template)]
#[template(path = "home-row.html")]
pub struct RowFragment {
    pub row: Row,
    pub items: Vec<AddonItem>,
}

/// A row of Kodi's home screen (Arctic Fuse, as set up on the TV), in the TV's order. The
/// library rows run the same queries as the skin's smart playlists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Row {
    ContinueWatching,
    OnDeck,
    InProgress,
    MyList,
    TrendingNow,
    RecentlyAdded,
    NewMovies,
    NewShows,
    RecentShows,
}

impl Row {
    pub const ALL: [Self; 9] = [
        Self::ContinueWatching,
        Self::OnDeck,
        Self::InProgress,
        Self::MyList,
        Self::TrendingNow,
        Self::RecentlyAdded,
        Self::NewMovies,
        Self::NewShows,
        Self::RecentShows,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::ContinueWatching => "Continue Watching",
            Self::OnDeck => "On Deck",
            Self::InProgress => "In-Progress",
            Self::MyList => "My List",
            Self::TrendingNow => "Trending Now",
            Self::RecentlyAdded => "Recently Added",
            Self::NewMovies => "New Movies",
            Self::NewShows => "New Shows",
            Self::RecentShows => "Recent Shows",
        }
    }

    /// The `/home/{slug}` URL segment; matches the serde name.
    pub fn slug(self) -> &'static str {
        match self {
            Self::ContinueWatching => "continue-watching",
            Self::OnDeck => "on-deck",
            Self::InProgress => "in-progress",
            Self::MyList => "my-list",
            Self::TrendingNow => "trending-now",
            Self::RecentlyAdded => "recently-added",
            Self::NewMovies => "new-movies",
            Self::NewShows => "new-shows",
            Self::RecentShows => "recent-shows",
        }
    }

    pub fn is_netflix(self) -> bool {
        matches!(self, Self::ContinueWatching | Self::MyList | Self::TrendingNow | Self::RecentlyAdded)
    }

    pub async fn load(self, kodi: &Kodi) -> Result<Vec<AddonItem>, KodiError> {
        Ok(match self {
            Self::ContinueWatching => netflix_row(kodi, "video_list/continueWatching/continueWatching/").await?,
            Self::OnDeck => cards(kodi.picked_movies(Pick::InProgress).await?),
            Self::InProgress => cards(kodi.picked_episodes(Pick::InProgress).await?),
            Self::MyList => netflix_row(kodi, "video_list_sorted/myList/queue/").await?,
            Self::TrendingNow => netflix_row(kodi, "video_list/currentTitles/trendingNow/").await?,
            Self::RecentlyAdded => netflix_row(kodi, "video_list_sorted/recentlyAdded/1592210/").await?,
            Self::NewMovies => cards(kodi.picked_movies(Pick::Unwatched).await?),
            Self::NewShows => cards(kodi.picked_tv_shows(Pick::Unwatched).await?),
            Self::RecentShows => cards(kodi.picked_tv_shows(Pick::RecentlyPlayed).await?),
        })
    }
}

fn cards<T>(items: Vec<T>) -> Vec<AddonItem>
where
    AddonItem: From<T>,
{
    items.into_iter().map(AddonItem::from).collect()
}

/// One of the Netflix add-on's lists (`list` is its path under `directory/`). Netflix only has
/// the lists on the profile's home page that day, and asking for a missing one pops an error
/// on the TV, so this checks the home page first and returns nothing if the list isn't there.
async fn netflix_row(kodi: &Kodi, list: &str) -> Result<Vec<AddonItem>, KodiError> {
    let path = format!("plugin://plugin.video.netflix/directory/{list}");
    let home = kodi.directory(Service::Netflix.home()).await?;
    if !home.iter().any(|e| e.file == path) {
        return Ok(vec![]);
    }
    Ok(netflix_cards(kodi.directory(&path).await?))
}

/// The titles of a Netflix list, without its menu entries ("Next page" and the like).
fn netflix_cards(entries: Vec<DirEntry>) -> Vec<AddonItem> {
    entries
        .into_iter()
        .filter(|e| e.filetype == FileType::File || e.kind == "tvshow")
        .map(|e| AddonItem::new(Service::Netflix, e))
        .collect()
}

/// The library's Apple TV segment: Apple TV+ plays in a browser on the TV, not in Kodi.
#[derive(Template)]
#[template(path = "browser.html")]
pub struct BrowserPage {
    pub site: Site,
    /// The browser is on the TV already.
    pub open: bool,
}

/// The Search tab while the browser is open (see `routes::browser_search`).
#[derive(Template)]
#[template(path = "browser-search.html")]
pub struct BrowserSearch {
    pub site: Site,
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
    pub id: i64,
    pub title: String,
    pub detail: String,
    pub poster: Option<String>,
    pub backdrop: Option<String>,
    pub next: Option<NextUp>,
    pub seasons: Vec<Season>,
}

/// An add-on the remote browses through Kodi, using the add-on's own menus. `Search` is
/// TMDb Helper, entered only through [`SearchPage`] results: it plays a title from Netflix
/// using the player files in Kodi's userdata, and fails when Netflix doesn't have it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Service {
    Netflix,
    AppleTv,
    Search,
}

impl Service {
    pub fn name(self) -> &'static str {
        match self {
            Self::Netflix => "Netflix",
            Self::AppleTv => "Apple TV",
            Self::Search => "Search",
        }
    }

    /// The `/addon/{slug}` URL segment; matches the serde name.
    pub fn slug(self) -> &'static str {
        match self {
            Self::Netflix => "netflix",
            Self::AppleTv => "appletv",
            Self::Search => "search",
        }
    }

    /// Where Back goes once the page trail is empty: the add-on's home, or the search page.
    pub fn root(self) -> &'static str {
        match self {
            Self::Netflix => "addon/netflix",
            Self::AppleTv => "addon/appletv",
            Self::Search => "search",
        }
    }

    /// The add-on's top menu (for Netflix, the active profile's home). The remote never links
    /// to TMDb Helper's own menu, but it is the safe default for a bare `addon/search`.
    pub fn home(self) -> &'static str {
        match self {
            Self::Netflix => "plugin://plugin.video.netflix/directory/home/",
            Self::AppleTv => "plugin://plugin.video.appletv/",
            Self::Search => TMDB_HELPER,
        }
    }

    /// Whether `path` is inside this add-on. Routes refuse anything else, so they can't be
    /// used to browse or play arbitrary files on the Kodi box.
    pub fn owns(self, path: &str) -> bool {
        let prefix = match self {
            Self::Netflix => "plugin://plugin.video.netflix/",
            Self::AppleTv => "plugin://plugin.video.appletv/",
            Self::Search => TMDB_HELPER,
        };
        path.starts_with(prefix)
    }

    /// Entries that need the TV (search types on its keyboard, sign-in) or don't apply here.
    fn hidden(self) -> &'static [&'static str] {
        match self {
            Self::Netflix => &["/directory/search/", "/directory/exported/", "/directory/profiles/"],
            Self::AppleTv => &["action=search", "action=sign_out"],
            Self::Search => &[],
        }
    }

    fn url(self, path: &str, title: &str) -> String {
        format!("addon/{}?path={}&title={}", self.slug(), percent_encode(path), percent_encode(title))
    }
}

const TMDB_HELPER: &str = "plugin://plugin.video.themoviedb.helper/";

/// TMDb Helper's search listing for `query`; `kind` is `movie` or `tv`.
pub fn tmdb_search_path(kind: &str, query: &str) -> String {
    format!("{TMDB_HELPER}?info=search&tmdb_type={kind}&query={}", percent_encode(query))
}

/// The library's Search segment: a search field and, once searched, TMDb's matching movies
/// and shows. Its `data-addon-url` makes it the start of the page trail, so Back from a
/// show's seasons returns to these results.
#[derive(Template)]
#[template(path = "search.html")]
pub struct SearchPage {
    pub online: bool,
    pub query: String,
    pub url: String,
    pub movies: Vec<AddonItem>,
    pub shows: Vec<AddonItem>,
}

/// One page of a streaming add-on, read through Kodi. Its entries are split by how they
/// render: titles as landscape cards, episodes as rows, and every other folder (menus,
/// categories, seasons, "Next page") as a plain list.
#[derive(Template)]
#[template(path = "addon.html")]
pub struct AddonPage {
    pub service: Service,
    pub online: bool,
    pub title: String,
    /// This page's own URL, which the client keeps so Back can return here.
    pub url: String,
    pub home: bool,
    pub titles: Vec<AddonItem>,
    pub episodes: Vec<AddonItem>,
    pub folders: Vec<AddonItem>,
}

/// One card or row: a title, episode or folder from an add-on, or a library item on Home.
pub struct AddonItem {
    pub title: String,
    /// Year for titles, running time for episodes.
    pub detail: String,
    pub art: Option<String>,
    pub progress: Option<String>,
    /// Playable items post to this URL; folders load it into the library pane.
    pub href: String,
    pub plays: bool,
}

#[derive(Template)]
#[template(path = "audio.html")]
pub struct AudioSheet {
    pub playing: bool,
    pub tracks: Vec<Choice>,
    pub subtitles: Vec<Choice>,
    pub subtitles_off: bool,
    pub outputs: Vec<Choice>,
    /// Paired Bluetooth headphones Kodi can't list yet because they aren't connected;
    /// `value` is the device address, which tapping connects.
    pub headphones: Vec<Choice>,
    /// The stream sink exists, so "Listen on this device" can be offered.
    pub can_listen: bool,
    /// Kodi (or in the browser, the whole system) is playing into the stream sink rather than
    /// a speaker.
    pub listening: bool,
    /// The Sync control's current setting, in words.
    pub sync_label: String,
    /// Sync follows the measured delay (`Listen::report_delay`) rather than Earlier/Later.
    pub sync_auto: bool,
    /// Kodi's sheet rather than the browser's: only Kodi has tracks and the Sync control,
    /// which sets Kodi's audio delay.
    pub kodi: bool,
}

pub struct Status {
    pub online: bool,
    /// The browser is on the TV in Kodi's place (see `browser`).
    pub browser: bool,
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
    /// Asks Kodi what it's doing, or while the browser is open, what the browser plays (over
    /// MPRIS). Any failure other than "offline" is shown as idle.
    pub async fn load(kodi: &Kodi, mpris: &Mpris) -> Self {
        let playback = match kodi.playback().await {
            Ok(p) => p,
            Err(KodiError::Offline) => {
                if browser::is_open().await {
                    let playing = mpris.playing().await.unwrap_or_else(|e| {
                        tracing::warn!("reading the browser's playback: {e}");
                        None
                    });
                    let now = playing.map(NowPlaying::from_browser);
                    return Self { online: false, browser: true, unreachable: false, muted: false, now };
                }
                let unreachable = kodi::process_running().await;
                return Self { online: false, browser: false, unreachable, muted: false, now: None };
            }
            Err(e) => {
                tracing::warn!("reading playback: {e}");
                None
            }
        };
        let muted = kodi.volume().await.map(|v| v.muted).unwrap_or(false);
        Self { online: true, browser: false, unreachable: false, muted, now: playback.map(NowPlaying::from) }
    }

    /// Value for the `data-state` attribute that CSS and the media controls key off. In the
    /// browser it's "playing"/"paused" while the page plays something, else "browser".
    pub fn state(&self) -> &'static str {
        if self.browser {
            return match &self.now {
                None => "browser",
                Some(n) if n.paused => "paused",
                Some(_) => "playing",
            };
        }
        match (&self.now, self.online) {
            (_, false) if self.unreachable => "unreachable",
            (_, false) => "off",
            (None, true) => "idle",
            (Some(n), true) if n.paused => "paused",
            (Some(_), true) => "playing",
        }
    }
}

impl NowPlaying {
    /// The browser's media session. Its artwork is a file Chrome writes, served by
    /// `/browser/art`; the file's name changes with the artwork, so it versions the URL.
    fn from_browser(p: mpris::Playing) -> Self {
        let art = p.art_file.as_deref().map(|f| format!("browser/art?v={}", percent_encode(f.rsplit('/').next().unwrap_or(f))));
        let (elapsed, total) = (p.position as u64, p.duration as u64);
        Self {
            poster: art.clone(),
            backdrop: art,
            title: p.title,
            subtitle: p.subtitle,
            elapsed: clock(elapsed),
            remaining: format!("-{}", clock(total.saturating_sub(elapsed))),
            percent: format!("{:.2}", (p.position / p.duration * 100.0).clamp(0.0, 100.0)),
            position: elapsed.min(total),
            duration: total,
            paused: p.paused,
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
            id: show.tvshowid,
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

impl AddonPage {
    /// `path` is the add-on folder these `entries` were listed from.
    pub fn new(service: Service, path: &str, title: String, home: bool, entries: Vec<DirEntry>) -> Self {
        let mut page = Self {
            url: service.url(path, &title),
            service,
            online: true,
            title,
            home,
            titles: vec![],
            episodes: vec![],
            folders: vec![],
        };
        let hidden = service.hidden();
        for e in entries.into_iter().filter(|e| !hidden.iter().any(|h| e.file.contains(h))) {
            let list = match (e.filetype, e.kind.as_str()) {
                (FileType::File, "episode") => &mut page.episodes,
                (FileType::File, _) | (FileType::Directory, "tvshow") => &mut page.titles,
                (FileType::Directory, _) => &mut page.folders,
            };
            list.push(AddonItem::new(service, e));
        }
        page
    }

    pub fn offline(service: Service) -> Self {
        Self {
            service,
            online: false,
            title: String::new(),
            url: String::new(),
            home: true,
            titles: vec![],
            episodes: vec![],
            folders: vec![],
        }
    }

    pub fn is_empty(&self) -> bool {
        self.titles.is_empty() && self.episodes.is_empty() && self.folders.is_empty()
    }
}

impl SearchPage {
    /// The field alone, before anything is searched (or while Kodi is off).
    pub fn blank(online: bool) -> Self {
        Self { online, query: String::new(), url: "search".into(), movies: vec![], shows: vec![] }
    }

    /// Results of [`tmdb_search_path`] for movies and for shows. TMDb Helper appends a
    /// "Next page" folder to each; the first page (20 of each) is plenty from a remote.
    pub fn new(query: String, movies: Vec<DirEntry>, shows: Vec<DirEntry>) -> Self {
        let keep = |entries: Vec<DirEntry>, kind: &str| -> Vec<AddonItem> {
            entries.into_iter().filter(|e| e.kind == kind).map(|e| AddonItem::new(Service::Search, e)).collect()
        };
        Self {
            online: true,
            url: format!("search?q={}", percent_encode(&query)),
            query,
            movies: keep(movies, "movie"),
            shows: keep(shows, "tvshow"),
        }
    }
}

impl AddonItem {
    fn new(service: Service, e: DirEntry) -> Self {
        let plays = e.filetype == FileType::File;
        let detail = match (e.kind.as_str(), &e.resume) {
            ("episode", Some(r)) if r.total >= 60.0 => format!("{} min", (r.total / 60.0).round()),
            ("episode", _) => String::new(),
            _ => year(e.year),
        };
        Self {
            href: if plays {
                format!("addon/{}/play?path={}", service.slug(), percent_encode(&e.file))
            } else {
                service.url(&e.file, &e.label)
            },
            art: ["landscape", "thumb", "poster"].iter().find_map(|k| e.art.get(*k)).map(|src| art_url(src)),
            progress: resume_percent(e.resume.as_ref()),
            title: match e.episode {
                Some(n) if e.kind == "episode" => {
                    format!("{n}. {}", if e.title.is_empty() { &e.label } else { &e.title })
                }
                _ => e.label,
            },
            detail,
            plays,
        }
    }
}

/// Home cards are landscape, like the add-ons' (see `title-card.html`).
fn wide_art(art: &std::collections::HashMap<String, String>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|k| art.get(*k)).map(|src| art_url(src))
}

impl From<Movie> for AddonItem {
    fn from(m: Movie) -> Self {
        Self {
            href: format!("movies/{}/play", m.movieid),
            detail: year(m.year),
            art: wide_art(&m.art, &["landscape", "fanart", "poster"]),
            progress: resume_percent(m.resume.as_ref()),
            title: m.title,
            plays: true,
        }
    }
}

impl From<TvShow> for AddonItem {
    fn from(s: TvShow) -> Self {
        Self {
            href: format!("shows/{}", s.tvshowid),
            detail: year(s.year),
            art: wide_art(&s.art, &["landscape", "fanart", "poster"]),
            progress: None,
            title: s.title,
            plays: false,
        }
    }
}

impl From<Episode> for AddonItem {
    fn from(e: Episode) -> Self {
        Self {
            href: format!("episodes/{}/play", e.episodeid),
            detail: format!("S{} · E{}", e.season, e.episode),
            art: wide_art(&e.art, &["thumb", "tvshow.landscape", "tvshow.fanart"]),
            progress: resume_percent(e.resume.as_ref()),
            title: if e.showtitle.is_empty() { e.title } else { e.showtitle },
            plays: true,
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
    /// `headphones` are the paired Bluetooth audio devices; those Kodi already lists as an
    /// output are left out, so each device appears once.
    pub async fn load(
        kodi: &Kodi,
        sync_earlier_ms: i32,
        sync_auto: bool,
        headphones: Vec<Headphones>,
    ) -> Result<Self, KodiError> {
        let playback = kodi.playback().await?;
        let audio_outputs = kodi.audio_outputs().await?;
        let mut sheet = Self::new(playback, audio_outputs, sync_earlier_ms, headphones, true);
        sheet.sync_auto = sync_auto;
        if sync_auto {
            sheet.sync_label = format!("Auto · {}", sheet.sync_label.to_lowercase());
        }
        Ok(sheet)
    }

    /// The browser's sheet: the system's outputs (PipeWire sinks, which Chrome plays to),
    /// Bluetooth headphones and "This device", without Kodi's tracks or Sync.
    pub async fn load_browser(headphones: Vec<Headphones>) -> Result<Self, sound::SoundError> {
        Ok(Self::new(None, sound::outputs().await?, 0, headphones, false))
    }

    fn new(
        playback: Option<Playback>,
        audio_outputs: AudioOutputs,
        sync_earlier_ms: i32,
        headphones: Vec<Headphones>,
        kodi: bool,
    ) -> Self {
        let can_listen =
            audio_outputs.options.iter().any(|o| o.value.as_str().is_some_and(listen::is_stream_device));
        let listening = listen::is_stream_device(&audio_outputs.current);
        let headphones = headphones
            .into_iter()
            .filter(|h| !audio_outputs.options.iter().any(|o| o.value.as_str().is_some_and(|v| h.address.is_output(v))))
            .map(|h| Choice {
                value: h.address.to_string(),
                detail: if h.connected { "Connected · not an output yet" } else { "Bluetooth · Not connected" }
                    .to_owned(),
                title: h.name,
                selected: false,
            })
            .collect();
        let mut sheet = Self {
            playing: false,
            tracks: vec![],
            subtitles: vec![],
            subtitles_off: true,
            outputs: output_choices(audio_outputs),
            headphones,
            can_listen,
            listening,
            sync_label: match sync_earlier_ms {
                0 => "No adjustment".to_owned(),
                ms if ms > 0 => format!("Sound {ms} ms earlier"),
                ms => format!("Sound {} ms later", -ms),
            },
            sync_auto: false,
            kodi,
        };
        if let Some(Playback { props, .. }) = playback {
            let current_audio = props.currentaudiostream.and_then(|s| s.index);
            let current_sub = props.currentsubtitle.and_then(|s| s.index).filter(|_| props.subtitleenabled);
            sheet.playing = true;
            sheet.tracks = props.audiostreams.iter().map(|s| audio_choice(s, current_audio)).collect();
            sheet.subtitles = props.subtitles.iter().map(|s| subtitle_choice(s, current_sub)).collect();
            sheet.subtitles_off = current_sub.is_none();
        }
        sheet
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
            } else if device.contains("bluez_output") {
                "Bluetooth".to_owned()
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Entries shaped like the Netflix add-on's `Files.GetDirectory` output.
    fn entries() -> Vec<DirEntry> {
        serde_json::from_value(json!([
            { "label": "My List", "file": "plugin://plugin.video.netflix/directory/video_list_sorted/myList/queue/", "filetype": "directory", "type": "unknown" },
            { "label": "Search", "file": "plugin://plugin.video.netflix/directory/search/search/", "filetype": "directory", "type": "unknown" },
            { "label": "Some Show", "file": "plugin://plugin.video.netflix/directory/show/1/", "filetype": "directory", "type": "tvshow", "year": 2019 },
            { "label": "Some Movie", "file": "plugin://plugin.video.netflix/play/movie/2/", "filetype": "file", "type": "movie",
              "resume": { "position": 30.0, "total": 120.0 } },
            { "label": "Pilot", "file": "plugin://plugin.video.netflix/play/show/1/season/3/episode/4/", "filetype": "file", "type": "episode",
              "episode": 1, "resume": { "position": 0.0, "total": 3141.0 } },
        ]))
        .unwrap()
    }

    fn titles(items: &[AddonItem]) -> Vec<&str> {
        items.iter().map(|i| i.title.as_str()).collect()
    }

    #[test]
    fn addon_page_sorts_entries_by_how_they_render() {
        let page = AddonPage::new(Service::Netflix, Service::Netflix.home(), "Netflix".into(), true, entries());

        assert_eq!(titles(&page.folders), ["My List"], "search is hidden");
        assert_eq!(titles(&page.titles), ["Some Show", "Some Movie"]);
        assert_eq!(titles(&page.episodes), ["1. Pilot"]);

        let (show, movie, episode) = (&page.titles[0], &page.titles[1], &page.episodes[0]);
        assert!(!show.plays && show.href.starts_with("addon/netflix?path="));
        assert!(movie.plays && movie.href.starts_with("addon/netflix/play?path="));
        assert_eq!(movie.progress.as_deref(), Some("25.0"));
        assert_eq!(episode.detail, "52 min");
    }

    #[test]
    fn apple_tv_hides_entries_that_need_the_tv_and_owns_only_its_paths() {
        let entries = serde_json::from_value(json!([
            { "label": "Featured", "file": "plugin://plugin.video.appletv/?action=featured", "filetype": "directory", "type": "unknown" },
            { "label": "Search", "file": "plugin://plugin.video.appletv/?action=search", "filetype": "directory", "type": "unknown" },
            { "label": "Sign out", "file": "plugin://plugin.video.appletv/?action=sign_out", "filetype": "directory", "type": "unknown" },
        ]))
        .unwrap();
        let page = AddonPage::new(Service::AppleTv, Service::AppleTv.home(), "Apple TV".into(), true, entries);

        assert_eq!(titles(&page.folders), ["Featured"]);
        assert!(page.folders[0].href.starts_with("addon/appletv?path="));
        assert!(!Service::AppleTv.owns("plugin://plugin.video.netflix/directory/home/"));
        assert!(!Service::Netflix.owns("/home/user/Movies/"));
    }

    #[test]
    fn search_keeps_only_titles_and_plays_them_through_tmdb_helper() {
        let movies = serde_json::from_value(json!([
            { "label": "The Break-Up", "file": "plugin://plugin.video.themoviedb.helper/?info=play&tmdb_type=movie&tmdb_id=9767", "filetype": "file", "type": "movie", "year": 2006 },
            { "label": "Next page", "file": "plugin://plugin.video.themoviedb.helper/?info=search&tmdb_type=movie&query=break&page=2", "filetype": "directory", "type": "unknown" },
        ]))
        .unwrap();
        let shows = serde_json::from_value(json!([
            { "label": "Severance", "file": "plugin://plugin.video.themoviedb.helper/?info=seasons&tmdb_type=tv&tmdb_id=95396", "filetype": "directory", "type": "tvshow" },
        ]))
        .unwrap();
        let page = SearchPage::new("break up".into(), movies, shows);

        assert_eq!(page.url, "search?q=break%20up");
        assert_eq!(titles(&page.movies), ["The Break-Up"]);
        assert!(
            page.movies[0].plays
                && page.movies[0]
                    .href
                    .starts_with("addon/search/play?path=plugin%3A%2F%2Fplugin.video.themoviedb.helper")
        );
        assert_eq!(titles(&page.shows), ["Severance"]);
        assert!(page.shows[0].href.starts_with("addon/search?path="));
    }

    #[test]
    fn home_cards_play_library_items_and_open_shows() {
        let movie: Movie = serde_json::from_value(json!({ "movieid": 17, "title": "Holidate", "year": 2020,
            "art": { "poster": "p.jpg", "landscape": "l.jpg" } })).unwrap();
        let show: TvShow = serde_json::from_value(json!({ "tvshowid": 7, "title": "Cobra Kai" })).unwrap();
        let episode: Episode = serde_json::from_value(json!({ "episodeid": 3, "title": "Pilot", "showtitle": "The Wire",
            "season": 1, "episode": 2 })).unwrap();

        let (movie, show, episode) = (AddonItem::from(movie), AddonItem::from(show), AddonItem::from(episode));
        assert!(movie.plays && movie.href == "movies/17/play");
        assert_eq!(movie.art.as_deref(), Some("art?src=l.jpg"), "landscape art first");
        assert!(!show.plays && show.href == "shows/7");
        assert!(episode.plays && episode.href == "episodes/3/play");
        assert_eq!((episode.title.as_str(), episode.detail.as_str()), ("The Wire", "S1 · E2"));
    }

    #[test]
    fn netflix_rows_keep_titles_but_not_menu_entries() {
        let mut list = entries();
        list.push(serde_json::from_value(json!({ "label": "Next page", "file": "plugin://plugin.video.netflix/directory/video_list/currentTitles/trendingNow/?page=2", "filetype": "directory", "type": "unknown" })).unwrap());

        assert_eq!(titles(&netflix_cards(list)), ["Some Show", "Some Movie", "1. Pilot"]);
    }

    #[test]
    fn episodes_are_numbered_once() {
        let entries = serde_json::from_value(json!([
            { "label": "1x01. Good News About Hell", "title": "Good News About Hell", "file": "plugin://plugin.video.themoviedb.helper/?info=play&tmdb_type=tv&tmdb_id=95396&season=1&episode=1", "filetype": "file", "type": "episode", "episode": 1 },
        ]))
        .unwrap();
        let page = AddonPage::new(Service::Search, Service::Search.home(), "Season 1".into(), false, entries);

        assert_eq!(titles(&page.episodes), ["1. Good News About Hell"]);
    }
}
