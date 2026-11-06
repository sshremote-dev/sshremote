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
- Planned: DXGI capture, macOS (ScreenCaptureKit), kitty graphics, chat.

## Build

```sh
cargo build --release
```

On Linux the agent needs `libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev pkg-config libopus-dev
libasound2-dev` and a C++ compiler for the bundled OpenH264. On Windows libopus is built from source
with CMake (WinLibs ships one). On Windows the GNU toolchain links the C++ runtime statically
(`.cargo/config.toml`), so `desk.exe` needs no MinGW DLLs.
Install the binary on the remote host as `~/.local/bin/desk` (on Windows, anywhere on `PATH`).

## Windows hosts

The OpenSSH service runs sessions outside the console session, where the screen cannot be read.
The agent notices this and starts a helper in the console session of the same user through a
one-shot scheduled task (`schtasks /it`), then relays the stream over two named pipes that only
that user and SYSTEM may open. The user must be logged on at the console. A helper that fails
writes its error to `%TEMP%\desk-agent.log`.

For unattended access, `desk --install-service` (elevated) installs an optional LocalSystem service.
The relay then asks it, over a pipe only Administrators and SYSTEM may open, to start the helper as
SYSTEM in the console session, which also works at the sign-in and lock screens when nobody is
logged on; the scheduled task remains the fallback. The service also raises Ctrl+Alt+Del (it sets
the SoftwareSASGeneration policy to allow services to). `desk --uninstall-service` removes both.
Users who are not administrators keep the scheduled-task path.

The viewer also runs inside a Windows ssh session (`desk --local --capture windows` typed in the
remote shell). There every byte normally passes through a pseudo console (ConPTY) that re-renders
text and drops sixel images, so the viewer duplicates the write end of the pseudo console host's
output pipe, which leads straight to sshd, and writes its images there. Keys still arrive through
the console. Pass `--cell WxH` when the client terminal's cell size is not 10x20.

## Window mode

`desk` with no arguments (or a double click on `desk.exe`) opens the window: a connect screen with
host, port, an optional password (handed to ssh through its askpass hook, never stored), the
quality, and host cards. A failed connection is explained with its likely cause and what to try.
The window has a dark and a light theme, remembered between runs. `desk --gui HOST` connects
straight away.

In a session the toolbar at the top shows the connection state (a green, yellow or red dot with the
round trip; hover for traffic, fps and quality) and icon buttons for:

- display: quality presets and custom colours, scale and fps; fit, stretch or a fixed zoom; the
  remote screen to show;
- full screen (F11; the toolbar then hides until the pointer reaches the top edge);
- special keys (Ctrl+Alt+Del, Win, Win+R, Win+E, Win+D, Alt+Tab, Alt+F4, Print Screen, Win plus
  the next key);
- files, chat, screenshot, pause and single frame;
- options: clipboard sharing, sound, system keys, toolbar auto-hide, theme and a full repaint;
- disconnect.

Messages such as saved screenshots and finished downloads appear as notifications at the bottom right.

Every key goes to the remote host as a separate press and release by its position, so held
modifiers, Shift+click, Ctrl+drag and games behave as on a local keyboard, and the remote keyboard
layout decides the characters. Text from input methods (Korean, Japanese, Chinese) is sent as it
is committed. Mouse back and forward buttons and horizontal wheels are passed through. Copied text
is shared both ways while the clipboard switch is on. Ctrl+Alt+Del works on GNOME, and on Windows
with the desk service installed.

On a Windows viewer, while the session window is in front, Win, Alt+Tab, Alt+Esc, Ctrl+Esc and
Print Screen are taken with a low-level keyboard hook (on a thread of its own, since Windows skips a
hook whose thread is busy) and go to the remote host instead of this PC; the toolbar's "System keys"
switch turns this off.

Files and folders dropped on the window are sent to the remote user's Downloads folder; a folder
keeps its structure. The toolbar's Files button opens a browser of the remote host's folders (home
first, hidden files on request); a selected file or folder is downloaded to this PC's Downloads
folder. Either way an existing file is never overwritten (the copy gets " (2)" and so on), names
cannot leave the target folder, and at most 1 MiB per transfer is unacknowledged, so a large file
does not hold up the screen. `--probe SECS --send FILE` and `--fetch REMOTE_PATH` test transfers
headless.

On Windows the window renders with Direct3D 12 (`WGPU_BACKEND` overrides it), which also works where
a GPU driver's Vulkan or OpenGL does not.

With several monitors on the remote host the toolbar shows a Screen menu (`Ctrl+] s` steps through
them in a terminal). On Windows each monitor is captured and the pointer placed on the whole virtual
desktop; on GNOME the agent records the chosen connector from Mutter's DisplayConfig.

The Sound switch (`--sound` in a terminal) plays the host's sound: WASAPI loopback of the default
output device on Windows, the default sink's monitor through PulseAudio or PipeWire on Linux, sent as
20 ms Opus packets at 64 kbit/s (about 8 KB/s, much less during silence) and played from a buffer
capped at 300 ms so the sound stays live. It is off until asked for.

## Viewing modes, screenshots and saving traffic

The View menu fits the remote screen to the window (proportions kept), stretches it to fill the
window, or shows it at a fixed 50-200% of its resolution; a fixed size larger than the window scrolls
when the pointer rests near an edge. Screenshot saves the remote screen as a PNG in Pictures. Pause
stops the picture (input still works) and Update fetches a single frame, so a slow link can be used
picture by picture. In a terminal the menu (Ctrl+]) offers the same: `x` screenshot, `p` pause,
`u` one frame, `o` sound, `c` chat, `g` download a remote path, `l` send a local file, `s` next screen,
`z` zoom; answers and messages appear in the status line.

## Chat

Chat (toolbar, or `Ctrl+] c` in a terminal) opens a small always-on-top window on the remote desktop
for the person sitting there; their replies come back to the viewer. Nothing is stored, and the remote
window closes itself shortly after the viewer leaves.

## Hosts

The connect screen shows favorites, recent hosts and the `Host` aliases of `~/.ssh/config` as
cards with the time of the last connection, a search box and a star to pin a host; ssh applies
their user, port and keys as usual. A click fills in the form, a double click connects. Hosts are
remembered only after a successful connection. `desk --term` offers the same list in a terminal.

## MCP

`desk mcp HOST` (or `desk mcp --local`, `desk mcp desk://...`) is a Model Context Protocol server on
stdio that lets an AI agent see and operate the remote desktop. Tools: `screenshot`, `click`, `move`,
`drag`, `type`, `key`, `scroll`, `wait`, `monitors`, `select_monitor`, `clipboard_get`,
`clipboard_set`. To keep tokens low the picture stream stays paused and a screenshot is taken only on
request, 1024 px wide by default, optionally a region and in grey; coordinates passed to the other
tools are in the latest screenshot's pixels and are mapped back to the screen by the server. Example
client configuration:

```json
{"mcpServers": {"desk": {"command": "desk", "args": ["mcp", "user@host"]}}}
```

On GNOME, text the keyboard layout cannot produce (Korean, emoji) is pasted through the clipboard;
the previous clipboard text is put back a second later unless something new was copied.

## Transports

`src/transport.rs` is the only place that knows how a viewer reaches an agent: a local child process,
`ssh -T`, or a direct link. Everything above it sees two byte streams.

Without ssh, `desk --serve` waits for viewers on TCP port 7070 and prints a one-time password; a viewer
connects with `desk://HOST` and that password (`--password` in a terminal, the password field in the
window). For a host behind NAT, `desk --relay` on any reachable machine pairs the two sides by an ID:
the host runs `desk --serve --via RELAY` and is reached as `desk://ID@RELAY` (port 7071).

Both ends run SPAKE2 on the password, so neither an eavesdropper nor the relay learns anything to
guess offline, and every byte after that is sealed with ChaCha20-Poly1305 under keys derived from the
exchange; a wrong password fails the sealed confirmation at once. Each failed attempt slows the server
down further and ten end it, so a nine-digit password cannot be ground down online. The relay only
copies bytes.

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

## Credits

The icons are from [Phosphor Icons](https://phosphoricons.com) (MIT), as packaged by
[egui-phosphor](https://github.com/amPerl/egui-phosphor); see `assets/fonts/LICENSE-phosphor.txt`.
