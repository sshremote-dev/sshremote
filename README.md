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
- Also: file transfer, a remote terminal, chat, sound, screenshots, sharing one window or application,
  and an MCP server.
- Planned: DXGI capture, macOS (ScreenCaptureKit), kitty graphics.

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
- share: the entire screen, one window or one application (see below);
- full screen (F11; the toolbar then hides until the pointer reaches the top edge);
- special keys (Ctrl+Alt+Del, Win, Win+R, Win+E, Win+D, Alt+Tab, Alt+F4, Print Screen, Win plus
  the next key);
- file transfer, terminal, chat, screenshot, pause and single frame;
- options: clipboard sharing, sound, system keys, theme and a full repaint;
- a pin that keeps the toolbar shown (a filled pin on a blue button); unpinned, the toolbar folds
  into a small tab hanging from the top edge, as in Jump Desktop, that opens it when the pointer
  reaches it; the tab and the toolbar's grip move it along the edge, and the place is remembered;
- disconnect.

Menus stay open while their sliders and switches are used, and the toolbar stays while a menu is open.
The host name and round trip take fixed widths, so the buttons do not shift as the numbers change.

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

## File transfer

The Files button opens a window of its own with this computer on the left and the remote host on the
right. Each side has a path bar, Up, Home and Drives, a list sortable by name, size and modification
time, multiple selection (Ctrl, Shift, Ctrl+A) and hidden files on request. Files and whole folders
go across with the arrow buttons, by dragging from one side to the other, by double click, or by
dropping them from the desktop on the window (uploaded into the remote folder shown). New folder,
rename (F2) and delete (Del, confirmed) work on both sides, and a transfer list shows progress with a
cancel button. Files dropped on the main window go to the remote Downloads folder as before.

Either way an existing file is never overwritten (the copy gets " (2)" and so on), names
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

## Terminal

The Terminal button opens shells on the remote host in a window with tabs. The agent starts the
user's login shell on a pseudo-terminal (PowerShell or cmd in a pseudo console on Windows) and the
bytes travel over the same connection, so it needs no second login and also works over `desk://`.
The window emulates xterm with Alacritty's terminal engine: colours, full-screen programs such as
vim, top and less, wide characters, scrollback (wheel, Shift+PageUp), selection with copy (Ctrl+C
with a selection, otherwise it interrupts) and paste, and text size buttons. For ssh hosts a button
opens the same host in the system's own terminal. In a terminal viewer, `Ctrl+] t` swaps the picture
for a remote shell that the terminal itself emulates; `exit` or Ctrl+] returns to the screen. A
Windows agent running as the service (SYSTEM) refuses shells; connect over ssh for one.

## Sharing a window or an application

As in a video call (Meet, Loom), the Share button lists what the remote host can send: its screens,
its open windows, and its applications (all Chrome windows, say). "Active window" and "Active
application" at the top of the lists share the window in front, or every window of its program,
without picking from a list. The picture is then only that
window or application, so it is smaller and cheaper; anything covering it is greyed out, clicks
outside it are dropped, and keys go only while it has the focus. A chip in the toolbar shows what is
shared and returns to the whole screen when clicked.

On Windows the list comes from the desktop's top-level windows, and the chosen window is brought to
the front. GNOME Shell gives its window list only to its own screen-sharing portal, so on GNOME the
list comes from the desk helper, a small GNOME Shell extension (`assets/gnome-helper`) that the
chooser offers to install; GNOME loads it at the next login on the remote host. The helper is
optional: without it GNOME offers the active window, while the active application and the lists need
it, because only it can tell where a program's other windows are. A window is then
recorded by Mutter on its own (its content shows even when covered), and an application is cut out of
the monitor picture. In a terminal viewer `Ctrl+] v` switches between the focused window and the whole
screen; MCP has `windows` and `share`.

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
`drag`, `type`, `key`, `scroll`, `wait`, `monitors`, `select_monitor`, `windows`, `share`,
`clipboard_get`, `clipboard_set`. Sharing just the window being worked on keeps screenshots small. To keep tokens low the picture stream stays paused and a screenshot is taken only on
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
terminal output itself crosses the network), `t` remote shell, `v` active window or whole screen,
`r` refresh. The status line (window title in `--gui`) shows traffic and round-trip time.

| Key | Resolution | Colours | fps |
|---|---|---|---|
| 0 | 50% | black and white | 5 |
| 1 | 75% | 16 greys | 10 |
| 2 | 100% | 256 colours | 20 |
| 3 | 100% | full colour | 30 |
| 4 | 100% | full colour | 60 |

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
