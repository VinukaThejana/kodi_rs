//! Browser mode: a website full screen on the TV in Kodi's place, driven from the remote through
//! a virtual keyboard and mouse.
//!
//! It exists for Apple TV+: Apple's licence server refuses the Widevine CDM as Kodi loads it
//! (status -1002), while the same CDM inside Chrome plays. The browser is Chrome in kiosk mode
//! inside cage (a one-window Wayland compositor), run as the transient user unit `tv-browser`,
//! the same way `/kodi/launch` runs Kodi. When the unit stops, however that happens (the remote,
//! Ctrl+Shift+Q on the laptop, a crash), its ExecStopPost starts Kodi again, unless the remote
//! turned the TV off.
//!
//! The virtual devices need write access to /dev/uinput; deploy/99-uinput.rules gives it to the
//! `input` group.

use std::{
    env,
    path::PathBuf,
    sync::{Arc, Mutex},
};

use evdev::{AttributeSet, EventType, InputEvent, KeyCode, RelativeAxisCode, uinput::VirtualDevice};
use tokio::process::Command;

use crate::kodi::{self, Volume, VolumeStep, percent_encode};

const UNIT: &str = "tv-browser";

#[derive(Debug, thiserror::Error)]
pub enum BrowserError {
    #[error("couldn't open the browser: {0}")]
    Launch(String),
    #[error("the remote can't type or point on the TV ({0}); see deploy/99-uinput.rules")]
    Input(std::io::Error),
    #[error("couldn't change the volume: {0}")]
    Volume(String),
}

pub type Result<T> = std::result::Result<T, BrowserError>;

/// The sites browser mode opens. Only these: the remote never opens an arbitrary URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Site {
    AppleTv,
}

impl Site {
    pub fn name(self) -> &'static str {
        match self {
            Self::AppleTv => "Apple TV",
        }
    }

    pub fn url(self) -> &'static str {
        match self {
            Self::AppleTv => "https://tv.apple.com/",
        }
    }
}

/// A tv.apple.com page the remote opens in the browser. Only public pages: the remote doesn't
/// read Apple's catalogue itself, it sends the TV where the site already lists it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Page {
    Home,
    Search,
}

impl Page {
    /// The page's URL; `term` fills in a search.
    pub fn url(self, term: Option<&str>) -> String {
        match (self, term.map(str::trim).filter(|t| !t.is_empty())) {
            (Self::Home, _) => Site::AppleTv.url().to_owned(),
            (Self::Search, None) => format!("{}search", Site::AppleTv.url()),
            (Self::Search, Some(term)) => format!("{}search?term={}", Site::AppleTv.url(), percent_encode(term)),
        }
    }
}

/// Whether the browser is on the TV.
pub async fn is_open() -> bool {
    Command::new("systemctl")
        .args(["--user", "is-active", "--quiet", UNIT])
        .status()
        .await
        .is_ok_and(|s| s.success())
}

/// Starts `site` full screen on the TV. Kodi must already be closed: both need the screen.
pub async fn open(site: Site) -> Result<()> {
    let home = env::var("HOME").unwrap_or_default();
    // Its own profile, so the Apple sign-in persists without touching any other Chrome profile.
    let profile = format!("--user-data-dir={home}/.config/chrome-tv");
    let kodi = kodi::LAUNCH.join(" ");
    let stop_post = format!(
        "ExecStopPost=/bin/sh -c 'if [ -e %t/{UNIT}-off ]; then rm -f %t/{UNIT}-off; else systemd-run --user --unit=kodi --collect --quiet {kodi}; fi'"
    );
    let out = Command::new("systemd-run")
        .args(["--user", &format!("--unit={UNIT}"), "--collect", "--quiet", "-p", &stop_post])
        // cage opens the screen and input devices directly: this unit has no logind session of
        // its own (the seat's session belongs to the console), but the user may use them.
        .args(["-E", "LIBSEAT_BACKEND=noop", "--", "cage", "--", "google-chrome-stable", "--kiosk"])
        .args(["--ozone-platform=wayland", "--no-first-run", "--no-default-browser-check", &profile, site.url()])
        .output()
        .await
        .map_err(|e| BrowserError::Launch(e.to_string()))?;
    if !out.status.success() {
        return Err(BrowserError::Launch(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(())
}

/// Closes the browser. Kodi comes back on its own unless `turn_off`.
pub async fn close(turn_off: bool) -> Result<()> {
    if turn_off {
        let runtime = env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(|| "/tmp".into());
        std::fs::write(runtime.join(format!("{UNIT}-off")), b"").map_err(|e| BrowserError::Launch(e.to_string()))?;
    }
    let out = Command::new("systemctl")
        .args(["--user", "stop", UNIT])
        .output()
        .await
        .map_err(|e| BrowserError::Launch(e.to_string()))?;
    if !out.status.success() {
        return Err(BrowserError::Launch(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(())
}

/// The system volume (PipeWire's default output). Kodi has its own volume, but it's closed while
/// the browser is open.
pub async fn step_volume(step: VolumeStep) -> Result<Volume> {
    let change = match step {
        VolumeStep::Up => "5%+",
        VolumeStep::Down => "5%-",
    };
    wpctl(&["set-volume", "--limit", "1.0", "@DEFAULT_AUDIO_SINK@", change]).await?;
    volume().await
}

pub async fn toggle_mute() -> Result<Volume> {
    wpctl(&["set-mute", "@DEFAULT_AUDIO_SINK@", "toggle"]).await?;
    volume().await
}

/// Reads `Volume: 0.40` or `Volume: 0.40 [MUTED]` from wpctl.
async fn volume() -> Result<Volume> {
    let out = wpctl(&["get-volume", "@DEFAULT_AUDIO_SINK@"]).await?;
    let level = out.split_whitespace().nth(1).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.0);
    Ok(Volume { volume: (level * 100.0).round().clamp(0.0, 100.0) as u8, muted: out.contains("[MUTED]") })
}

async fn wpctl(args: &[&str]) -> Result<String> {
    let out = Command::new("wpctl").args(args).output().await.map_err(|e| BrowserError::Volume(e.to_string()))?;
    if !out.status.success() {
        return Err(BrowserError::Volume(String::from_utf8_lossy(&out.stderr).trim().to_owned()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A key the remote presses in the browser.
#[derive(Debug, Clone, Copy)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Enter,
    Space,
    /// The browser's Back (Alt+Left).
    Back,
}

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Button {
    Left,
    Right,
}

/// The remote's virtual keyboard and mouse, created once and kept: the compositor only uses a
/// device it has seen appear, and creating one per press would lose the first events.
#[derive(Clone, Default)]
pub struct Input {
    devices: Arc<Mutex<Option<Devices>>>,
}

struct Devices {
    keyboard: VirtualDevice,
    mouse: VirtualDevice,
    /// Hi-res wheel units not yet sent as a whole notch, for scrollers that read only notches.
    wheel_rest: (i32, i32),
}

/// One wheel notch in REL_*_HI_RES units, as the kernel defines it.
const NOTCH: i32 = 120;

impl Input {
    /// Creates the devices now if /dev/uinput allows it, so they are ready (and seen by the
    /// compositor) before the first press. Later calls retry if this fails.
    pub fn new() -> Self {
        let input = Self::default();
        if let Err(e) = input.with(|_| Ok(())) {
            tracing::warn!("{e}");
        }
        input
    }

    fn with(&self, f: impl FnOnce(&mut Devices) -> std::io::Result<()>) -> Result<()> {
        let mut guard = self.devices.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            *guard = Some(Devices::create().map_err(BrowserError::Input)?);
        }
        f(guard.as_mut().expect("created above")).map_err(BrowserError::Input)
    }

    pub fn press(&self, key: Key) -> Result<()> {
        let (code, alt) = match key {
            Key::Up => (KeyCode::KEY_UP, false),
            Key::Down => (KeyCode::KEY_DOWN, false),
            Key::Left => (KeyCode::KEY_LEFT, false),
            Key::Right => (KeyCode::KEY_RIGHT, false),
            Key::Enter => (KeyCode::KEY_ENTER, false),
            Key::Space => (KeyCode::KEY_SPACE, false),
            Key::Back => (KeyCode::KEY_LEFT, true),
        };
        let modifier = alt.then_some(KeyCode::KEY_LEFTALT);
        self.with(|d| d.tap(code, modifier))
    }

    /// Types `text` as key presses on a US layout. Characters it has no key for are skipped.
    pub fn type_text(&self, text: &str) -> Result<()> {
        self.with(|d| {
            for c in text.chars() {
                match us_key(c) {
                    Some((code, shift)) => d.tap(code, shift.then_some(KeyCode::KEY_LEFTSHIFT))?,
                    None => tracing::debug!("no key for {c:?}"),
                }
            }
            Ok(())
        })
    }

    /// Loads `url` in the open browser, as a person would: Ctrl+L, the URL, Enter.
    pub fn navigate(&self, url: &str) -> Result<()> {
        self.with(|d| d.tap(KeyCode::KEY_L, Some(KeyCode::KEY_LEFTCTRL)))?;
        self.type_text(url)?;
        self.press(Key::Enter)
    }

    pub fn move_by(&self, dx: i32, dy: i32) -> Result<()> {
        self.with(|d| {
            d.mouse.emit(&[
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_X.0, dx),
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_Y.0, dy),
            ])
        })
    }

    pub fn click(&self, button: Button) -> Result<()> {
        let code = match button {
            Button::Left => KeyCode::BTN_LEFT,
            Button::Right => KeyCode::BTN_RIGHT,
        };
        self.with(|d| {
            d.mouse.emit(&[InputEvent::new(EventType::KEY.0, code.0, 1)])?;
            d.mouse.emit(&[InputEvent::new(EventType::KEY.0, code.0, 0)])
        })
    }

    /// Scrolls by `dx`, `dy` in hi-res wheel units (120 per notch); positive `dy` scrolls up.
    pub fn scroll(&self, dx: i32, dy: i32) -> Result<()> {
        self.with(|d| {
            let (rest_x, rest_y) = (d.wheel_rest.0 + dx, d.wheel_rest.1 + dy);
            let (notches_x, notches_y) = (rest_x / NOTCH, rest_y / NOTCH);
            d.wheel_rest = (rest_x % NOTCH, rest_y % NOTCH);
            d.mouse.emit(&[
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL_HI_RES.0, dy),
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL_HI_RES.0, dx),
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_WHEEL.0, notches_y),
                InputEvent::new(EventType::RELATIVE.0, RelativeAxisCode::REL_HWHEEL.0, notches_x),
            ])
        })
    }
}

impl Devices {
    fn create() -> std::io::Result<Self> {
        let mut keys = AttributeSet::<KeyCode>::new();
        for code in KEYBOARD_KEYS {
            keys.insert(code);
        }
        let keyboard = VirtualDevice::builder()?.name("kodi-remote keyboard").with_keys(&keys)?.build()?;

        let mut buttons = AttributeSet::<KeyCode>::new();
        for code in [KeyCode::BTN_LEFT, KeyCode::BTN_RIGHT, KeyCode::BTN_MIDDLE] {
            buttons.insert(code);
        }
        let mut axes = AttributeSet::<RelativeAxisCode>::new();
        for axis in [
            RelativeAxisCode::REL_X,
            RelativeAxisCode::REL_Y,
            RelativeAxisCode::REL_WHEEL,
            RelativeAxisCode::REL_HWHEEL,
            RelativeAxisCode::REL_WHEEL_HI_RES,
            RelativeAxisCode::REL_HWHEEL_HI_RES,
        ] {
            axes.insert(axis);
        }
        let mouse = VirtualDevice::builder()?
            .name("kodi-remote mouse")
            .with_keys(&buttons)?
            .with_relative_axes(&axes)?
            .build()?;
        Ok(Self { keyboard, mouse, wheel_rest: (0, 0) })
    }

    /// Presses and releases `code`, holding `modifier` around it.
    fn tap(&mut self, code: KeyCode, modifier: Option<KeyCode>) -> std::io::Result<()> {
        let key = |code: KeyCode, down: bool| InputEvent::new(EventType::KEY.0, code.0, i32::from(down));
        if let Some(m) = modifier {
            self.keyboard.emit(&[key(m, true)])?;
        }
        self.keyboard.emit(&[key(code, true)])?;
        self.keyboard.emit(&[key(code, false)])?;
        if let Some(m) = modifier {
            self.keyboard.emit(&[key(m, false)])?;
        }
        Ok(())
    }
}

/// Every key the virtual keyboard can press: what [`Input::press`] and [`us_key`] use.
const KEYBOARD_KEYS: [KeyCode; 61] = {
    use KeyCode as K;
    [
        K::KEY_A, K::KEY_B, K::KEY_C, K::KEY_D, K::KEY_E, K::KEY_F, K::KEY_G, K::KEY_H, K::KEY_I, K::KEY_J,
        K::KEY_K, K::KEY_L, K::KEY_M, K::KEY_N, K::KEY_O, K::KEY_P, K::KEY_Q, K::KEY_R, K::KEY_S, K::KEY_T,
        K::KEY_U, K::KEY_V, K::KEY_W, K::KEY_X, K::KEY_Y, K::KEY_Z, K::KEY_1, K::KEY_2, K::KEY_3, K::KEY_4,
        K::KEY_5, K::KEY_6, K::KEY_7, K::KEY_8, K::KEY_9, K::KEY_0, K::KEY_MINUS, K::KEY_EQUAL,
        K::KEY_LEFTBRACE, K::KEY_RIGHTBRACE, K::KEY_BACKSLASH, K::KEY_SEMICOLON, K::KEY_APOSTROPHE,
        K::KEY_GRAVE, K::KEY_COMMA, K::KEY_DOT, K::KEY_SLASH, K::KEY_SPACE, K::KEY_ENTER, K::KEY_ESC,
        K::KEY_TAB, K::KEY_BACKSPACE, K::KEY_UP, K::KEY_DOWN, K::KEY_LEFT, K::KEY_RIGHT,
        K::KEY_LEFTSHIFT, K::KEY_LEFTCTRL, K::KEY_LEFTALT, K::KEY_HOME, K::KEY_END,
    ]
};

/// The key, and whether Shift is held, that types `c` on a US layout.
fn us_key(c: char) -> Option<(KeyCode, bool)> {
    use KeyCode as K;
    const LETTERS: [KeyCode; 26] = [
        K::KEY_A, K::KEY_B, K::KEY_C, K::KEY_D, K::KEY_E, K::KEY_F, K::KEY_G, K::KEY_H, K::KEY_I, K::KEY_J,
        K::KEY_K, K::KEY_L, K::KEY_M, K::KEY_N, K::KEY_O, K::KEY_P, K::KEY_Q, K::KEY_R, K::KEY_S, K::KEY_T,
        K::KEY_U, K::KEY_V, K::KEY_W, K::KEY_X, K::KEY_Y, K::KEY_Z,
    ];
    const DIGITS: [KeyCode; 10] =
        [K::KEY_0, K::KEY_1, K::KEY_2, K::KEY_3, K::KEY_4, K::KEY_5, K::KEY_6, K::KEY_7, K::KEY_8, K::KEY_9];
    // Each punctuation key with its unshifted and shifted character.
    const PUNCTUATION: [(KeyCode, char, char); 11] = [
        (K::KEY_MINUS, '-', '_'),
        (K::KEY_EQUAL, '=', '+'),
        (K::KEY_LEFTBRACE, '[', '{'),
        (K::KEY_RIGHTBRACE, ']', '}'),
        (K::KEY_BACKSLASH, '\\', '|'),
        (K::KEY_SEMICOLON, ';', ':'),
        (K::KEY_APOSTROPHE, '\'', '"'),
        (K::KEY_GRAVE, '`', '~'),
        (K::KEY_COMMA, ',', '<'),
        (K::KEY_DOT, '.', '>'),
        (K::KEY_SLASH, '/', '?'),
    ];
    const SHIFTED_DIGITS: &str = ")!@#$%^&*(";
    match c {
        'a'..='z' => Some((LETTERS[c as usize - 'a' as usize], false)),
        'A'..='Z' => Some((LETTERS[c as usize - 'A' as usize], true)),
        '0'..='9' => Some((DIGITS[c as usize - '0' as usize], false)),
        ' ' => Some((K::KEY_SPACE, false)),
        '\t' => Some((K::KEY_TAB, false)),
        '\n' => Some((K::KEY_ENTER, false)),
        _ => SHIFTED_DIGITS
            .find(c)
            .map(|i| (DIGITS[i], true))
            .or_else(|| {
                PUNCTUATION.iter().find_map(|&(code, plain, shifted)| {
                    (c == plain).then_some((code, false)).or((c == shifted).then_some((code, true)))
                })
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pages_are_apple_tv_urls() {
        assert_eq!(Page::Home.url(Some("ignored")), "https://tv.apple.com/");
        assert_eq!(Page::Search.url(Some("  ")), "https://tv.apple.com/search");
        assert_eq!(Page::Search.url(Some("ted lasso")), "https://tv.apple.com/search?term=ted%20lasso");
    }

    #[test]
    fn types_a_us_layout() {
        assert_eq!(us_key('a'), Some((KeyCode::KEY_A, false)));
        assert_eq!(us_key('Q'), Some((KeyCode::KEY_Q, true)));
        assert_eq!(us_key('0'), Some((KeyCode::KEY_0, false)));
        assert_eq!(us_key('@'), Some((KeyCode::KEY_2, true)));
        assert_eq!(us_key(')'), Some((KeyCode::KEY_0, true)));
        assert_eq!(us_key('?'), Some((KeyCode::KEY_SLASH, true)));
        assert_eq!(us_key('.'), Some((KeyCode::KEY_DOT, false)));
        assert_eq!(us_key('é'), None);
    }

    #[test]
    fn every_typed_key_is_on_the_virtual_keyboard() {
        let printable = (' '..='~').chain(['\t', '\n']);
        for c in printable {
            let (code, _) = us_key(c).unwrap_or_else(|| panic!("no key for {c:?}"));
            assert!(KEYBOARD_KEYS.contains(&code), "{c:?} uses {code:?}, which the keyboard lacks");
        }
    }
}
