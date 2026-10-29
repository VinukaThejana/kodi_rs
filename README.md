# Kodi Remote

A self-hosted web remote for [Kodi](https://kodi.tv), styled after the Apple TV remote.
It runs on the same Linux machine as Kodi and works in any browser on your network, so
your phone or laptop becomes the remote. On an iPhone you can add it to the home screen
and it behaves like an app.

- **Remote**: a touch surface you can tap or swipe (like the Siri Remote's clickpad),
  plus Back, Home, Play/Pause, Mute and volume. Keyboard works too on a laptop.
- **Now Playing**: artwork, a progress bar you can drag to seek, skip back 10s or
  forward 30s, and stop.
- **Library**: browse movies and TV shows with posters and resume points. Shows have
  seasons, episodes and a "next episode" button. Tap anything to play it on the TV.
- **Audio menu**: switch audio track, subtitles and sound output (TV, speakers, …).
- **System media controls**: what's playing shows up on the iPhone lock screen, in
  Control Center and in macOS Now Playing, and play/pause/skip from there control Kodi.
- **Listen on this device** (optional): stream Kodi's sound to the browser you're
  using, for example to listen on a laptop while watching the TV, with a **Sync**
  control to line the sound up with the picture.
- **Turn Kodi on and off** from the remote (see [Turn On / Turn Off](#turn-on--turn-off)).

It's a small Rust server ([Axum](https://github.com/tokio-rs/axum)) that renders HTML
for [htmx](https://htmx.org) and talks to Kodi over its JSON-RPC API. Everything it
serves, including CSS, JavaScript and icons, is compiled into one binary.

## Contents

1. [Requirements](#requirements)
2. [Enable Kodi's remote control API](#1-enable-kodis-remote-control-api)
3. [Build and install the remote](#2-build-and-install-the-remote)
4. [Open it on your phone or laptop](#3-open-it-on-your-phone-or-laptop)
5. [Optional: serve it under a path with nginx](#4-optional-serve-it-under-a-path-with-nginx)
6. [Optional: Listen on this device](#5-optional-listen-on-this-device)
7. [Turn On / Turn Off](#turn-on--turn-off)
8. [Using it](#using-it)
9. [Configuration](#configuration)
10. [Troubleshooting](#troubleshooting)
11. [Security](#security)
12. [Development](#development)

## Requirements

- **Linux with systemd**, on the machine that runs Kodi. The remote talks to Kodi on
  `127.0.0.1`, so it has to run on the same machine.
- **Kodi 21 (Omega) or newer.** Older versions may work, but the remote was built
  against Kodi 21's JSON-RPC API (version 13).
- **Rust 1.85 or newer** to build it ([rustup.rs](https://rustup.rs)).
- For **Listen on this device** only:
  - PipeWire, with `pipewire-pulse`
  - `ffmpeg` built with PulseAudio input and the `libopus` encoder (most distro
    packages are)
  - [go2rtc](https://github.com/AlexxIT/go2rtc), a single self-contained binary

## 1. Enable Kodi's remote control API

In Kodi, go to **Settings → Services → Control** and set:

| Setting | Value |
|---|---|
| Allow remote control via HTTP | **On** |
| Port | `8080` (the default; change `KODI_URL` if you use another) |
| Username | `kodi` (or anything; set `KODI_USER` to match) |
| Password | **set one**. Kodi refuses HTTP control without a password |

"Allow remote control from applications on other systems" can stay **off**. Only the
remote server talks to Kodi, and it does so locally.

Check it works (replace the password):

```sh
curl -u kodi:YOUR_PASSWORD -H 'Content-Type: application/json' \
  -d '{"jsonrpc":"2.0","id":1,"method":"JSONRPC.Ping"}' http://127.0.0.1:8080/jsonrpc
# {"id":1,"jsonrpc":"2.0","result":"pong"}
```

## 2. Build and install the remote

```sh
git clone https://github.com/VinukaThejana/kodi_rs.git
cd kodi_rs
cargo build --release
install -Dm755 target/release/kodi-remote ~/.local/bin/kodi-remote
```

Store Kodi's password in a file only you can read:

```sh
install -d -m700 ~/.config/kodi-remote
( umask 077; echo 'KODI_PASSWORD=YOUR_PASSWORD' > ~/.config/kodi-remote/env )
```

Add `KODI_USER=…`, `KODI_URL=…` or `BIND=…` to the same file if you changed them (see
[Configuration](#configuration)).

Run it as a systemd **user** service, so it starts with your user session and restarts
if it crashes:

```sh
install -Dm644 deploy/kodi-remote.service ~/.config/systemd/user/kodi-remote.service
systemctl --user daemon-reload
systemctl --user enable --now kodi-remote
```

To have it start **at boot**, even when nobody is logged in, turn on lingering for your
user (a one-time setting):

```sh
sudo loginctl enable-linger "$USER"
```

Check it's up:

```sh
systemctl --user status kodi-remote
journalctl --user -u kodi-remote -f    # logs
```

## 3. Open it on your phone or laptop

Browse to `http://<kodi-machine-ip>:8090`, for example `http://192.168.1.13:8090`.
Find the machine's address with `ip -4 addr`.

- **iPhone**: in Safari, tap Share → **Add to Home Screen**. It then opens full-screen
  with its own icon, like an app.
- **Laptop**: the remote shows in a sidebar with your library beside it.

If the page doesn't load from another device but works on the Kodi machine itself, a
firewall is probably blocking port 8090.

## 4. Optional: serve it under a path with nginx

The remote uses only relative URLs, so it works under a sub-path such as
`http://<ip>/kodi/` behind a reverse proxy. For nginx, add this inside your `server { … }`
block:

```nginx
location = /kodi {
    return 301 /kodi/;
}

location /kodi/ {
    proxy_pass http://127.0.0.1:8090/;   # the trailing slash strips /kodi
    proxy_set_header Host $host;
    proxy_set_header X-Real-IP $remote_addr;
    proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
    proxy_read_timeout 60s;              # "Turn On" waits up to 20s for Kodi
}
```

Then `sudo nginx -t && sudo systemctl reload nginx`. The trailing slash matters:
`/kodi/` works, and the first block redirects plain `/kodi` there.

## 5. Optional: Listen on this device

This sends Kodi's sound to the browser instead of the TV, with roughly a quarter of a
second of delay. That's useful for listening on a laptop while watching the TV, for
example late at night. It works like this:

```
Kodi ──▶ "Kodi Remote stream" (virtual PipeWire output)
              │ monitor
              ▼
   ffmpeg (Opus) ──▶ go2rtc ──WebRTC──▶ browser
```

Kodi plays into a virtual sound output. go2rtc records it and streams it to the browser
over WebRTC. go2rtc only runs ffmpeg while someone is listening.

### a. Create the virtual output

```sh
install -Dm644 deploy/pipewire-kodi-remote-sink.conf \
  ~/.config/pipewire/pipewire.conf.d/60-kodi-remote-sink.conf
```

That makes it permanent from the next login or reboot. To create it right now without
restarting PipeWire, which would interrupt anything playing:

```sh
pactl load-module module-null-sink sink_name=kodi_remote \
  'sink_properties=device.description="Kodi Remote stream"' rate=48000 channels=2
```

The output must be named `kodi_remote`; the remote looks for that name.

### b. Install go2rtc

Download `go2rtc_linux_amd64` (or the build for your CPU) from the
[go2rtc releases](https://github.com/AlexxIT/go2rtc/releases). GitHub shows a SHA-256
digest for each file; compare it with `sha256sum` before installing. Built and tested
with **v1.9.14**.

```sh
install -Dm755 go2rtc_linux_amd64 ~/.local/bin/go2rtc
install -Dm644 deploy/go2rtc.yaml ~/.config/go2rtc/go2rtc.yaml
install -Dm644 deploy/go2rtc.service ~/.config/systemd/user/go2rtc.service
systemctl --user daemon-reload
systemctl --user enable --now go2rtc
```

go2rtc's API (port 1984) and RTSP (port 8554) only listen on `127.0.0.1`. Browsers need
to reach **port 8555 (UDP and TCP)** on the Kodi machine for the audio itself, so open
it in your firewall if you run one.

### c. Use it

In the remote, open **Audio → Sound Output → This device**. The TV goes quiet and the
sound plays in that browser, at that device's volume. A **Sync** row appears with
**Earlier** / **Later** buttons that move the sound 25 ms at a time to line it up
with lips on screen. The setting is saved in `~/.local/state/kodi-remote/sync-ms`
and re-applied to each new video.

Pick another output (for example the TV) to switch back. If you just close the tab,
the remote notices nobody is listening and moves the sound back after about 30 seconds,
so the TV is never left silent. The page reconnects by itself after Wi-Fi drops or the
laptop waking from sleep, and if you reload it, your first tap resumes the sound.

## Turn On / Turn Off

**Turn Off** closes Kodi through its API and works with any setup.

**Turn On** starts Kodi as a transient systemd user unit:

```sh
systemd-run --user --unit=kodi --collect kodi --standalone --windowing=gbm
```

That fits a **dedicated TV box without a desktop**: Kodi draws directly to the screen
(GBM). Your user needs to be in the `video` and `input` groups. If Kodi runs inside a
desktop session (X11 or Wayland) instead, start Kodi the way you normally do and just
don't use Turn On. Everything else works the same. The command is in `launch()` in
`src/routes.rs` if you want to adapt it.

Turn On refuses to start a second Kodi if one is already running but its API isn't
answering (see "Not Responding" under [Troubleshooting](#troubleshooting)).

## Using it

**Touch surface**: tap the centre to select, tap an edge to move, or swipe. A long
swipe keeps stepping, which is handy for scrolling lists.

**Keyboard** (laptop, while the page is focused):

| Key | Action |
|---|---|
| Arrow keys | Move |
| Return | Select |
| Delete, Esc | Back |
| H | Home |
| Space | Play / pause |
| `+` / `−` (or fn + F12 / F11) | Volume up / down |
| M (or fn + F10) | Mute |

macOS keeps the plain volume keys for itself; with fn held they arrive as F10–F12.

**Lock screen and media keys**: tap anything on the page once (browsers only allow
media controls after a tap), then play something. It shows up in iOS Control Center,
on the lock screen and in macOS Now Playing.

**Library links**: `…/#library` opens straight to the library and `…/#remote` to the
remote. Handy for bookmarks.

## Configuration

Environment variables, set in `~/.config/kodi-remote/env` when running as a service:

| Variable | Default | Meaning |
|---|---|---|
| `KODI_PASSWORD` | *(required)* | Kodi's web server password |
| `KODI_USER` | `kodi` | Kodi's web server username |
| `KODI_URL` | `http://127.0.0.1:8080` | Where Kodi's web server listens |
| `BIND` | `0.0.0.0:8090` | Address and port the remote listens on |
| `GO2RTC_URL` | `http://127.0.0.1:1984` | go2rtc's API, for Listen on this device |

After changing it: `systemctl --user restart kodi-remote`.

## Troubleshooting

**The remote says "Off" but Kodi is running.** The remote can't reach Kodi's API.
Check step 1, and that `KODI_URL`, `KODI_USER` and `KODI_PASSWORD` match. Test with
the `curl` command in step 1.

**The remote says "Not Responding".** Kodi is running, but its API isn't reachable. The
usual cause is a second Kodi that took port 8080, so the one on screen couldn't start
its web server. Kodi only claims the port at startup. Fix it in Kodi under Settings →
Services → Control by switching "Allow remote control via HTTP" off and on, or quit
Kodi and start it once.

**Buttons do nothing / the TV doesn't react.** Make sure only one Kodi is running
(`pgrep -a kodi.bin`). If there are two, the remote may be controlling one you can't see.

**"Audio streaming isn't running on the PC".** go2rtc isn't running or reachable. Check
`systemctl --user status go2rtc`. The remote refuses to switch to This device in that
case, so the TV keeps its sound.

**"This device" isn't in the Audio menu.** The `kodi_remote` virtual output doesn't
exist (step 5a), or Kodi hasn't noticed it yet. It appears within a few seconds of
being created.

**Listening is silent or choppy.** Check `journalctl --user -u go2rtc`, and that port
8555 is reachable from the listening device. The stream is Opus at 48 kHz in 10 ms
frames, and ffmpeg re-stamps the audio by sample count (`asetpts`). Without that,
Safari conceals audio at every timestamp wobble and the sound gets choppy.

**The Audio menu hangs or says Kodi is off while Kodi is clearly running.** Kodi's
audio engine is stuck. This happened here after a failed attempt to use an AirPlay
output. Restart Kodi.

**Logs**:

```sh
journalctl --user -u kodi-remote -f   # every action and every failed request
journalctl --user -u go2rtc -f
less ~/.kodi/temp/kodi.log
```

## Security

The remote has **no login**. Anyone who can reach port 8090, or port 8555 for the audio
stream, can control Kodi and listen to it. That's the same exposure as Kodi's own web
remote, and fine on a home network, but **don't expose these ports to the internet**.
If you need remote access, put it behind a VPN (WireGuard, Tailscale) or an
authenticating reverse proxy.

Kodi's password stays on the server in `~/.config/kodi-remote/env`; browsers never see
it, because artwork is proxied through the remote.

## Development

```
src/
  main.rs     configuration and startup
  kodi.rs     typed client for Kodi's JSON-RPC API and image endpoint
  views.rs    Askama templates' view models (Kodi data → what the page shows)
  routes.rs   HTTP routes: pages, htmx fragments, actions
  listen.rs   "Listen on this device": go2rtc signalling, watchdog, Sync
templates/    Askama HTML templates (htmx attributes live here)
assets/       CSS, JS, vendored htmx 2.0.4 + idiomorph 0.7.3, icon
deploy/       systemd user units, go2rtc config, PipeWire sink
```

Run it locally against a Kodi instance:

```sh
KODI_PASSWORD=… cargo run
```

Templates and assets are compiled into the binary, so rebuild after editing them. To
deploy a change to the installed service:

```sh
cargo build --release && install -m755 target/release/kodi-remote ~/.local/bin/ \
  && systemctl --user restart kodi-remote
```
