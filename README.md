# desk

Remote desktop over plain SSH, viewed inside a terminal or a window. No relay, ID server or
open port: the viewer runs `ssh -T HOST desk --agent`, the agent sends only changed rectangles
in one long-lived zstd stream, and the viewer keeps an exact framebuffer copy and repaints
damaged cells locally (sixel in a terminal, a native window with `--gui`).

## Status

- Agent: GNOME Wayland (Mutter RemoteDesktop/ScreenCast over D-Bus, PipeWire via GStreamer),
  Windows (GDI capture, SendInput), synthetic test desktop.
- Viewer: sixel terminals (Windows Terminal, foot, WezTerm, VTE with sixel) and a native window,
  keyboard and mouse, live presets and an auto preset.
- Planned: DXGI capture, macOS (ScreenCaptureKit), kitty graphics.

## Build

```sh
cargo build --release
```

On Linux the agent needs `libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev pkg-config` and a C++
compiler for the bundled OpenH264. On Windows the GNU toolchain links the C++ runtime statically
(`.cargo/config.toml`), so `desk.exe` needs no MinGW DLLs.
Install the binary on the remote host as `~/.local/bin/desk` (on Windows, anywhere on `PATH`).

## Windows hosts

The OpenSSH service runs sessions outside the console session, where the screen cannot be read.
The agent notices this and starts a helper in the console session of the same user through a
one-shot scheduled task (`schtasks /it`), then relays the stream over two named pipes that only
that user and SYSTEM may open. The user must be logged on at the console. A helper that fails
writes its error to `%TEMP%\desk-agent.log`.

The viewer also runs inside a Windows ssh session (`desk --local --capture windows` typed in the
remote shell). There every byte normally passes through a pseudo console (ConPTY) that re-renders
text and drops sixel images, so the viewer duplicates the write end of the pseudo console host's
output pipe, which leads straight to sshd, and writes its images there. Keys still arrive through
the console. Pass `--cell WxH` when the client terminal's cell size is not 10x20.

## Use

```sh
desk HOST                 # auto preset, under 100 KB/s
desk HOST --gui           # the same in a window, up to true colour
desk HOST --preset 2      # fixed: full size, 256 colours, 20 fps
desk HOST --preset 0      # half size, 1-bit, 5 fps
desk HOST --budget 30     # auto, under 30 KB/s
desk HOST --mode gray16 --scale 60 --fps 10
desk --local              # built-in test desktop, no ssh
desk HOST --probe 5 --dump out.ppm   # headless: print traffic, save the reconstructed screen
```

Ctrl+] opens the menu: `q` quit, `0`-`4` presets, `a` auto, `m` colour mode, `+`/`-` scale, `f` fps,
`z` zoom (enlarge a reduced capture to the terminal; off by default inside an ssh session, where the
terminal output itself crosses the network), `r` refresh. The status line (window title in `--gui`) shows traffic and round-trip time.

| Preset | Scale | Colour | fps |
|---|---|---|---|
| 0 glance | 50% | mono | 5 |
| 1 lean | 75% | gray16 | 10 |
| 2 balanced | 100% | 256c | 20 |
| 3 crisp | 100% | true | 30 |
| 4 exact | 100% | true | 60 |

Sixel allows 256 colours per image, so in a terminal true colour is drawn with a palette made
for each repainted rectangle: exact when it has at most 256 colours, median cut otherwise.
True-colour tiles are sent with a median (LOCO-I) predictor before zstd. When a band of the screen
scrolls, the agent sends a move for what the viewer already has and only the newly exposed rows.

With 256 colours or true colour, an area that keeps changing (a playing video, an animation) becomes
a video region: after about 160x160 pixels of tiles changed in five of the last eight frames, that
area is sent as H.264 (OpenH264, bit rate from its size, the fps cap and the colour mode) and the
rest of the screen stays lossless. When the area has been still for 1.2 s it is sent losslessly
once more. On the synthetic desktop a 320x240 animation at 30 fps took 62% less traffic in true
colour and 36% less in 256 colours than lossless tiles.

Auto starts at preset 2. Each second it compares received traffic with the budget and the
round trip of a ping that queues behind frames: two seconds over budget or slow steps down,
a quiet spell steps up, and the wait before climbing back to a level that failed doubles.
A climb whose full-screen resend alone exceeds the budget caps auto below that level for ten
minutes.
