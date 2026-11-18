//! HTTP routes. Pages and fragments render HTML for htmx; actions return `204` with an
//! `HX-Trigger` header so the page refreshes the now-playing card (and shows a HUD for volume).

use std::time::Duration;

use askama::Template;
use axum::{
    Form, Router,
    extract::{FromRef, Path, Query, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::json;
use tokio::process::Command;

use crate::{
    bluetooth::{Address, Bluetooth, BluetoothError},
    kodi::{self, Kodi, KodiError, NavKey, Subtitle, Volume, VolumeStep},
    listen::{self, Listen, ListenError},
    views::{
        AddonPage, AudioSheet, Choice, IndexPage, MovieCard, MoviesFragment, Service, ShowCard, ShowPage,
        ShowsFragment, Status, StatusFragment,
    },
};

/// Everything handlers share. Handlers extract just the part they need (`State<Kodi>`,
/// `State<Listen>`) via `FromRef`, or all of it for the audio sheet.
#[derive(Clone, FromRef)]
pub struct AppState {
    pub kodi: Kodi,
    pub listen: Listen,
    pub bluetooth: Bluetooth,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/status", get(status))
        .route("/movies", get(movies))
        .route("/movies/{id}/play", post(play_movie))
        .route("/shows", get(shows))
        .route("/shows/{id}", get(show))
        .route("/episodes/{id}/play", post(play_episode))
        .route("/addon/{service}", get(addon))
        .route("/addon/{service}/play", post(play_addon))
        .route("/input/text", post(send_text))
        .route("/input/{key}", post(nav))
        .route("/player/play-pause", post(play_pause))
        .route("/player/stop", post(stop))
        .route("/player/skip", post(skip))
        .route("/player/seek", post(seek))
        .route("/volume/{step}", post(volume))
        .route("/mute", post(mute))
        .route("/audio", get(audio))
        .route("/audio/track", post(set_track))
        .route("/audio/subtitle", post(set_subtitle))
        .route("/audio/output", post(set_output))
        .route("/audio/bluetooth", post(connect_headphones))
        .route("/listen/start", post(listen_start))
        .route("/listen/offer", post(listen_offer))
        .route("/listen/state", get(listen_state))
        .route("/listen/sync", post(listen_sync))
        .route("/debug/webrtc", post(debug_webrtc))
        .route("/kodi/launch", post(launch))
        .route("/kodi/quit", post(quit))
        .route("/art", get(art))
        .route("/assets/{file}", get(asset))
        .route("/apple-touch-icon.png", get(touch_icon))
        .layer(middleware::from_fn(log_actions))
        .with_state(state)
}

/// Logs every action (non-GET) and every failed request. Polls and assets stay quiet.
async fn log_actions(req: Request, next: Next) -> Response {
    let (method, path) = (req.method().clone(), req.uri().path().to_owned());
    let started = std::time::Instant::now();
    let res = next.run(req).await;
    let status = res.status();
    if method != Method::GET || !status.is_success() {
        tracing::info!("{method} {path} -> {} in {}ms", status.as_u16(), started.elapsed().as_millis());
    }
    res
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Kodi(#[from] KodiError),
    #[error(transparent)]
    Render(#[from] askama::Error),
    #[error("{0}")]
    Launch(String),
    #[error(transparent)]
    Listen(ListenError),
    #[error(transparent)]
    Bluetooth(#[from] BluetoothError),
    #[error("not a path in the {0} add-on")]
    ForeignPath(&'static str),
}

impl From<ListenError> for AppError {
    fn from(e: ListenError) -> Self {
        match e {
            ListenError::Kodi(e) => Self::Kodi(e),
            other => Self::Listen(other),
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = match &self {
            Self::Kodi(KodiError::Offline) | Self::Listen(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Bluetooth(BluetoothError::NotPaired) | Self::ForeignPath(_) => StatusCode::BAD_REQUEST,
            Self::Bluetooth(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Kodi(KodiError::Slow) => StatusCode::GATEWAY_TIMEOUT,
            Self::Kodi(_) => StatusCode::BAD_GATEWAY,
            Self::Render(_) | Self::Launch(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        tracing::warn!("{self}");
        let message = match self {
            Self::Kodi(KodiError::Rpc { message, .. }) => message,
            Self::Kodi(KodiError::Http(_)) => "Kodi didn't respond".to_owned(),
            other => other.to_string(),
        };
        (status, message).into_response()
    }
}

type Result<T, E = AppError> = std::result::Result<T, E>;

fn render(t: impl Template) -> Result<Html<String>> {
    Ok(Html(t.render()?))
}

/// Empty response that tells the page to re-poll the now-playing card.
fn refreshed() -> impl IntoResponse {
    (StatusCode::NO_CONTENT, [("HX-Trigger", "refresh")])
}

/// Like [`refreshed`], plus a `hud` event carrying the volume so the page can flash it.
fn volume_hud(v: Volume) -> impl IntoResponse {
    let trigger = json!({ "refresh": null, "hud": { "level": v.volume, "muted": v.muted } });
    (StatusCode::NO_CONTENT, [("HX-Trigger", trigger.to_string())])
}

async fn index(State(kodi): State<Kodi>) -> Result<Html<String>> {
    render(IndexPage { status: Status::load(&kodi).await })
}

async fn status(State(kodi): State<Kodi>) -> Result<Html<String>> {
    render(StatusFragment { status: Status::load(&kodi).await })
}

async fn movies(State(kodi): State<Kodi>) -> Result<Html<String>> {
    let fragment = match kodi.movies().await {
        Ok(movies) => MoviesFragment { online: true, movies: movies.into_iter().map(MovieCard::from).collect() },
        Err(KodiError::Offline) => MoviesFragment { online: false, movies: vec![] },
        Err(e) => return Err(e.into()),
    };
    render(fragment)
}

async fn play_movie(State(kodi): State<Kodi>, Path(id): Path<i64>) -> Result<impl IntoResponse> {
    kodi.play_movie(id).await?;
    Ok(refreshed())
}

async fn shows(State(kodi): State<Kodi>) -> Result<Html<String>> {
    let fragment = match kodi.tv_shows().await {
        Ok(shows) => ShowsFragment { online: true, shows: shows.into_iter().map(ShowCard::from).collect() },
        Err(KodiError::Offline) => ShowsFragment { online: false, shows: vec![] },
        Err(e) => return Err(e.into()),
    };
    render(fragment)
}

async fn show(State(kodi): State<Kodi>, Path(id): Path<i64>) -> Result<Html<String>> {
    let (show, episodes) = tokio::try_join!(kodi.tv_show(id), kodi.episodes(id))?;
    render(ShowPage::new(show, episodes))
}

async fn play_episode(State(kodi): State<Kodi>, Path(id): Path<i64>) -> Result<impl IntoResponse> {
    kodi.play_episode(id).await?;
    Ok(refreshed())
}

#[derive(Deserialize)]
struct AddonQuery {
    path: Option<String>,
    title: Option<String>,
}

/// `path` if it is inside `service`'s add-on, so these routes can't reach other files on Kodi.
fn owned(service: Service, path: String) -> Result<String> {
    if service.owns(&path) { Ok(path) } else { Err(AppError::ForeignPath(service.name())) }
}

/// One folder of a streaming add-on; its home menu when no path is given.
async fn addon(
    State(kodi): State<Kodi>,
    Path(service): Path<Service>,
    Query(q): Query<AddonQuery>,
) -> Result<Html<String>> {
    let path = owned(service, q.path.unwrap_or_else(|| service.home().to_owned()))?;
    let home = path == service.home();
    let title = q.title.filter(|_| !home).unwrap_or_else(|| service.name().to_owned());
    let page = match kodi.directory(&path).await {
        Ok(entries) => AddonPage::new(service, &path, title, home, entries),
        Err(KodiError::Offline) => AddonPage::offline(service),
        Err(e) => return Err(e.into()),
    };
    render(page)
}

#[derive(Deserialize)]
struct PlayQuery {
    path: String,
}

async fn play_addon(
    State(kodi): State<Kodi>,
    Path(service): Path<Service>,
    Query(q): Query<PlayQuery>,
) -> Result<impl IntoResponse> {
    kodi.play_file(&owned(service, q.path)?).await?;
    Ok(refreshed())
}

async fn nav(State(kodi): State<Kodi>, Path(key): Path<NavKey>) -> Result<impl IntoResponse> {
    kodi.nav(key).await?;
    Ok(refreshed())
}

#[derive(Deserialize)]
struct TextForm {
    text: String,
    done: bool,
}

/// Sent on every keystroke (`done=false`, mirrored live on the TV) and once on submit.
/// Plain `204`: typing doesn't change playback, so there's no need to re-poll the status card.
async fn send_text(State(kodi): State<Kodi>, Form(f): Form<TextForm>) -> Result<StatusCode> {
    kodi.send_text(&f.text, f.done).await?;
    Ok(StatusCode::NO_CONTENT)
}

async fn play_pause(State(kodi): State<Kodi>) -> Result<impl IntoResponse> {
    kodi.play_pause().await?;
    Ok(refreshed())
}

async fn stop(State(kodi): State<Kodi>) -> Result<impl IntoResponse> {
    kodi.stop().await?;
    Ok(refreshed())
}

/// A relative jump, capped at ten minutes either way.
#[derive(Deserialize)]
#[serde(try_from = "i64")]
struct Seconds(i64);

impl TryFrom<i64> for Seconds {
    type Error = &'static str;

    fn try_from(n: i64) -> Result<Self, Self::Error> {
        if (-600..=600).contains(&n) { Ok(Self(n)) } else { Err("skip must be within ±600 seconds") }
    }
}

#[derive(Deserialize)]
struct SkipForm {
    seconds: Seconds,
}

async fn skip(State(kodi): State<Kodi>, Form(f): Form<SkipForm>) -> Result<impl IntoResponse> {
    kodi.skip(f.seconds.0).await?;
    Ok(refreshed())
}

#[derive(Deserialize)]
#[serde(try_from = "f64")]
struct Percent(f64);

impl TryFrom<f64> for Percent {
    type Error = &'static str;

    fn try_from(n: f64) -> Result<Self, Self::Error> {
        if (0.0..=100.0).contains(&n) { Ok(Self(n)) } else { Err("percent must be within 0–100") }
    }
}

#[derive(Deserialize)]
struct SeekForm {
    percent: Percent,
}

async fn seek(State(kodi): State<Kodi>, Form(f): Form<SeekForm>) -> Result<impl IntoResponse> {
    kodi.seek_to(f.percent.0).await?;
    Ok(refreshed())
}

async fn volume(State(kodi): State<Kodi>, Path(step): Path<VolumeStep>) -> Result<impl IntoResponse> {
    Ok(volume_hud(kodi.step_volume(step).await?))
}

async fn mute(State(kodi): State<Kodi>) -> Result<impl IntoResponse> {
    Ok(volume_hud(kodi.toggle_mute().await?))
}

async fn audio(State(app): State<AppState>) -> Result<Html<String>> {
    render(load_sheet(&app).await?)
}

/// The audio sheet as it stands. Bluetooth trouble only hides the headphones rows.
async fn load_sheet(app: &AppState) -> Result<AudioSheet> {
    let headphones = app.bluetooth.headphones().await.unwrap_or_else(|e| {
        tracing::debug!("listing Bluetooth headphones: {e}");
        vec![]
    });
    Ok(AudioSheet::load(&app.kodi, app.listen.sync_earlier_ms(), headphones).await?)
}

#[derive(Deserialize)]
struct TrackForm {
    index: i64,
}

/// Renders the audio sheet once it reflects a change just sent to Kodi. Kodi applies stream
/// switches asynchronously (~0.5s), so an immediate re-read still shows the old selection.
/// Gives up after ~1.5s and renders whatever Kodi reports.
async fn sheet_once(app: &AppState, applied: impl Fn(&AudioSheet) -> bool) -> Result<Html<String>> {
    let mut sheet = load_sheet(app).await?;
    for _ in 0..10 {
        if applied(&sheet) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
        sheet = load_sheet(app).await?;
    }
    render(sheet)
}

fn is_selected(choices: &[Choice], value: &str) -> bool {
    choices.iter().any(|c| c.selected && c.value == value)
}

async fn set_track(State(app): State<AppState>, Form(f): Form<TrackForm>) -> Result<Html<String>> {
    app.kodi.set_audio_stream(f.index).await?;
    let index = f.index.to_string();
    sheet_once(&app, |s| is_selected(&s.tracks, &index)).await
}

#[derive(Deserialize)]
struct SubtitleForm {
    index: Subtitle,
}

async fn set_subtitle(State(app): State<AppState>, Form(f): Form<SubtitleForm>) -> Result<Html<String>> {
    app.kodi.set_subtitle(f.index).await?;
    match f.index {
        Subtitle::Off => sheet_once(&app, |s| s.subtitles_off).await,
        Subtitle::Track(index) => {
            let index = index.to_string();
            sheet_once(&app, |s| is_selected(&s.subtitles, &index)).await
        }
    }
}

#[derive(Deserialize)]
struct OutputForm {
    value: String,
}

async fn set_output(State(app): State<AppState>, Form(f): Form<OutputForm>) -> Result<Html<String>> {
    app.kodi.set_audio_output(&f.value).await?;
    // Picking "System Default" must cancel switching back to headphones; see `switch_back`.
    app.bluetooth.remember_choice(&f.value);
    sheet_once(&app, |s| is_selected(&s.outputs, &f.value)).await
}

#[derive(Deserialize)]
struct HeadphonesForm {
    address: Address,
}

/// Connects paired Bluetooth headphones (taking them from e.g. a phone) and switches Kodi's
/// sound to them once Kodi lists their output. Kodi's sound returns to them if they drop out
/// (see [`Bluetooth::switch_back`]).
async fn connect_headphones(State(app): State<AppState>, Form(f): Form<HeadphonesForm>) -> Result<Html<String>> {
    let name = app.bluetooth.connect(&f.address).await?;
    let Some(output) = wait_for_output(&app.kodi, &f.address, Duration::from_secs(10)).await? else {
        return Err(BluetoothError::NotInKodi(name).into());
    };
    app.kodi.set_audio_output(&output).await?;
    sheet_once(&app, |s| is_selected(&s.outputs, &output)).await
}

/// Polls Kodi's output list until the headphones' sink shows up (PipeWire adds it a moment
/// after the Bluetooth connection), returning its Kodi device id, or `None` after `timeout`.
async fn wait_for_output(kodi: &Kodi, address: &Address, timeout: Duration) -> Result<Option<String>> {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let outputs = kodi.audio_outputs().await?;
        let found = outputs.options.iter().filter_map(|o| o.value.as_str()).find(|v| address.is_output(v));
        if let Some(device) = found {
            return Ok(Some(device.to_owned()));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(None);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Routes Kodi's sound into the stream; the page connects to it via [`listen_offer`].
async fn listen_start(State(app): State<AppState>) -> Result<Html<String>> {
    app.listen.start(&app.kodi).await?;
    sheet_once(&app, |s| s.listening).await
}

/// One tap of "Earlier" (+1) or "Later" (-1) on the Sync control.
#[derive(Deserialize)]
#[serde(try_from = "i32")]
struct SyncStep(i32);

impl TryFrom<i32> for SyncStep {
    type Error = &'static str;

    fn try_from(n: i32) -> Result<Self, Self::Error> {
        if n == 1 || n == -1 { Ok(Self(n)) } else { Err("step must be 1 or -1") }
    }
}

#[derive(Deserialize)]
struct SyncForm {
    step: SyncStep,
}

/// Moves the streamed sound earlier or later against the TV picture, effective immediately.
async fn listen_sync(State(app): State<AppState>, Form(f): Form<SyncForm>) -> Result<Html<String>> {
    app.listen.nudge_sync(f.step.0);
    // Nothing playing is fine: the sync keeper applies it when something starts.
    if let Err(e) = app.listen.apply_sync(&app.kodi).await {
        tracing::debug!("applying sync now: {e}");
    }
    render(load_sheet(&app).await?)
}

/// `"true"` while Kodi plays into the stream, so a reloaded page knows whether to resume.
async fn listen_state(State(kodi): State<Kodi>) -> Result<&'static str> {
    let streaming = listen::is_stream_device(&kodi.audio_output().await?);
    Ok(if streaming { "true" } else { "false" })
}

/// Temporary: logs the listening browser's WebRTC stats, which only the device can see.
async fn debug_webrtc(body: String) -> StatusCode {
    let body: String = body.chars().take(1000).collect();
    tracing::info!("webrtc stats: {body}");
    StatusCode::NO_CONTENT
}

/// WebRTC signalling: the browser POSTs its SDP offer and gets go2rtc's SDP answer back.
async fn listen_offer(State(listen): State<Listen>, offer: String) -> Result<String> {
    Ok(listen.answer(offer).await?)
}

/// Starts Kodi fullscreen on the TV as a transient systemd user unit, then waits for its API.
///
/// Refuses when a Kodi process already exists: a second instance can't get the screen or
/// the API port, and would only leave a hidden copy for the remote to talk to. Runs `kodi`
/// rather than `kodi-standalone`, whose crash-restart loop revives Kodi without GPU access.
async fn launch(State(kodi): State<Kodi>) -> Result<impl IntoResponse> {
    if !kodi.is_online().await {
        if kodi::process_running().await {
            return Err(AppError::Launch(
                "Kodi is already running but its remote control isn't reachable. Restart Kodi on the TV.".into(),
            ));
        }
        let out = Command::new("systemd-run")
            .args(["--user", "--unit=kodi", "--collect", "--quiet", "kodi", "--standalone", "--windowing=gbm"])
            .output()
            .await
            .map_err(|e| AppError::Launch(format!("couldn't start Kodi: {e}")))?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(AppError::Launch(format!("couldn't start Kodi: {}", stderr.trim())));
        }
        if !wait_until(&kodi, true, Duration::from_secs(20)).await {
            return Err(AppError::Launch("Kodi didn't come up within 20 seconds".into()));
        }
    }
    Ok((StatusCode::NO_CONTENT, [("HX-Trigger", "refresh, launched")]))
}

async fn quit(State(kodi): State<Kodi>) -> Result<impl IntoResponse> {
    kodi.quit().await?;
    wait_until(&kodi, false, Duration::from_secs(8)).await;
    Ok(refreshed())
}

/// Polls until Kodi's online state matches `online`; returns whether it did before `timeout`.
async fn wait_until(kodi: &Kodi, online: bool, timeout: Duration) -> bool {
    let poll = async {
        while kodi.is_online().await != online {
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
    };
    tokio::time::timeout(timeout, poll).await.is_ok()
}

#[derive(Deserialize)]
struct ArtQuery {
    src: String,
}

/// Proxies Kodi artwork, which needs Kodi's credentials the browser doesn't have.
async fn art(State(kodi): State<Kodi>, Query(q): Query<ArtQuery>) -> Result<Response> {
    if !q.src.starts_with("image://") {
        return Ok(StatusCode::BAD_REQUEST.into_response());
    }
    let (content_type, bytes) = kodi.image(&q.src).await?;
    // Kodi labels most artwork application/octet-stream, so trust the bytes over the header.
    let content_type = sniff_image(&bytes).map(str::to_owned).or(content_type);
    let mut res = ([(header::CACHE_CONTROL, "public, max-age=604800, immutable")], bytes).into_response();
    if let Some(ct) = content_type.and_then(|ct| ct.parse().ok()) {
        res.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    Ok(res)
}

fn sniff_image(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0xFF, 0xD8, 0xFF, ..] => Some("image/jpeg"),
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [b'R', b'I', b'F', b'F', _, _, _, _, b'W', b'E', b'B', b'P', ..] => Some("image/webp"),
        _ => None,
    }
}

/// Serves the CSS/JS/icon compiled into the binary, so deploying is a single file.
async fn asset(Path(file): Path<String>) -> Response {
    let (body, content_type): (&'static [u8], _) = match file.as_str() {
        "app.css" => (include_bytes!("../assets/app.css"), "text/css"),
        "app.js" => (include_bytes!("../assets/app.js"), "text/javascript"),
        "htmx.min.js" => (include_bytes!("../assets/htmx.min.js"), "text/javascript"),
        "idiomorph-ext.min.js" => (include_bytes!("../assets/idiomorph-ext.min.js"), "text/javascript"),
        "apple-touch-icon.png" => (include_bytes!("../assets/apple-touch-icon.png"), "image/png"),
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, content_type), (header::CACHE_CONTROL, "no-cache")], body).into_response()
}

/// iOS probes the site root for a home-screen icon when the page doesn't name one.
async fn touch_icon() -> Response {
    asset(Path("apple-touch-icon.png".to_owned())).await
}
