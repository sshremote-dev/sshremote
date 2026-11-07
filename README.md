# sshremote

Remote control of an SSH host: its screen, keyboard and mouse, files, shell and sound, from a
terminal or a window, over the same SSH login you already use. No relay, ID server or open port: the
viewer runs `ssh -T HOST sshremote --agent`, the agent sends only changed rectangles
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
(`.cargo/config.toml`), so `sshremote.exe` needs no MinGW DLLs.
Install the binary on the SSH host as `~/.local/bin/sshremote` (on Windows, anywhere on `PATH`).

## Windows hosts

The OpenSSH service runs sessions outside the console session, where the screen cannot be read.
The agent notices this and starts a helper in the console session of the same user through a
one-shot scheduled task (`schtasks /it`), then relays the stream over two named pipes that only
that user and SYSTEM may open. The user must be logged on at the console. A helper that fails
writes its error to `%TEMP%\sshremote-agent.log`.

For unattended access, `sshremote --install-service` (elevated) installs an optional LocalSystem service.
The relay then asks it, over a pipe only Administrators and SYSTEM may open, to start the helper as
SYSTEM in the console session, which also works at the sign-in and lock screens when nobody is
logged on; the scheduled task remains the fallback. The service also raises Ctrl+Alt+Del (it sets
the SoftwareSASGeneration policy to allow services to). `sshremote --uninstall-service` removes both.
Users who are not administrators keep the scheduled-task path.

The viewer also runs inside a Windows ssh session (`sshremote --local --capture windows` typed in the
remote shell). There every byte normally passes through a pseudo console (ConPTY) that re-renders
text and drops sixel images, so the viewer duplicates the write end of the pseudo console host's
output pipe, which leads straight to sshd, and writes its images there. Keys still arrive through
the console. Pass `--cell WxH` when the client terminal's cell size is not 10x20.

## Window mode

`sshremote` with no arguments (or a double click on `sshremote.exe`, which opens no console window)
shows the home screen, laid out like AnyDesk: the app's mark and name at the top, a "Connect to a host"
field, and below it the connections (favorites, recent hosts and the `Host` aliases of
`~/.ssh/config`) with a search box. Only the list scrolls.

Connecting opens an SSH terminal by default (the Terminal button, Enter, or a double click on a
host). The `...` button next to it, and the `...` menu of each host, open the remote screen, or the
host in the system's own terminal. The gear shows the connection options: port, a password for the
remote screen (handed to ssh through its askpass hook; "Remember" keeps it in the system's password
store), the screen quality, the agent command and the capture backend. A failed connection is explained with its likely cause and what to
try. The header holds a local terminal button, the tools menu (SSH keys, SSH hosts, local shells,
the built-in test desktop) and the dark or light theme, remembered between runs. `sshremote --gui
HOST` opens the remote screen straight away; `--terminal` opens it with the host's terminal shown.

### Administrator rights

On Windows the window asks for administrator rights (UAC) when it starts, and goes on without them
when they are refused. The header shows "Administrator" or "Standard user"; a click on the latter
asks again. With the rights, local terminal tabs run elevated and the tools menu can install or
remove the sshremote service on this computer (so it can be reached at the sign-in and lock screens
and receive Ctrl+Alt+Del); without them those items are shown disabled. "Ask for administrator
rights at start" in the tools menu turns the prompt off. An elevated window cannot take files
dragged from Explorer (Windows blocks it); the file window still transfers them.

In a session the toolbar at the top shows the connection state (a green, yellow or red dot with the
round trip; hover for traffic, fps and quality) and icon buttons for:

- display: quality presets and custom colours, scale and fps; fit, stretch or a fixed zoom; the
  remote screen to show;
- share: the entire screen, one window or one application (see below);
- full screen (F11; the toolbar then hides until the pointer reaches the top edge);
- special keys (Ctrl+Alt+Del, lock screen, Win, Win+R, Win+E, Win+D, Alt+Tab, Alt+F4, Print Screen,
  Win plus the next key);
- file transfer, terminal, chat, screenshot, pause and single frame; the terminal button swaps the
  picture for the host's terminal in the same window (and back, with the shells kept running); its
  context menu gives the terminal a window of its own or closes the shells;
- options: view only (watch without sending keys, clicks or the wheel; a badge on the picture says
  so), clipboard sharing, sound, system keys, theme and a full repaint;
- a pin that keeps the toolbar shown (a filled pin on a blue button); unpinned, the toolbar folds
  into a small tab hanging from the top edge, as in Jump Desktop, that opens it when the pointer
  reaches it; the tab and the toolbar's grip move it along the edge, and the place is remembered;
- disconnect.

Menus stay open while their sliders and switches are used, and the toolbar stays while a menu is open.
The host name and round trip take fixed widths, so the buttons do not shift as the numbers change.

Messages such as saved screenshots and finished downloads appear as notifications at the bottom right.

Every key goes to the SSH host as a separate press and release by its position, so held
modifiers, Shift+click, Ctrl+drag and games behave as on a local keyboard, and the remote keyboard
layout decides the characters. Text from input methods (Korean, Japanese, Chinese) is sent as it
is committed. Mouse back and forward buttons and horizontal wheels are passed through. Copied text
is shared both ways while the clipboard switch is on. Ctrl+Alt+Del works on GNOME, and on Windows
with the sshremote service installed.

On a Windows viewer, while the session window is in front and the pointer is over the picture (as in
RustDesk), Win, Alt+Tab, Alt+Esc, Ctrl+Esc and
Print Screen are taken with a low-level keyboard hook (on a thread of its own, since Windows skips a
hook whose thread is busy) and go to the SSH host instead of this PC; the toolbar's "System keys"
switch turns this off.

## File transfer

The Files button opens a window of its own with this computer on the left and the SSH host on the
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

With several monitors on the SSH host the toolbar shows a Screen menu (`Ctrl+] s` steps through
them in a terminal). On Windows each monitor is captured and the pointer placed on the whole virtual
desktop; on GNOME the agent records the chosen connector from Mutter's DisplayConfig.

The Sound switch (`--sound` in a terminal) plays the host's sound: WASAPI loopback of the default
output device on Windows, the default sink's monitor through PulseAudio or PipeWire on Linux, sent as
20 ms Opus packets at 64 kbit/s (about 8 KB/s, much less during silence) and played from a buffer
capped at 300 ms so the sound stays live. It is off until asked for.

## Terminal

The terminal is one window with tabs, in the manner of Windows Terminal. The `+` button opens the
default profile and the arrow beside it lists the others: the shell on the SSH host of a remote-screen
session, and local shells (Windows PowerShell, PowerShell 7, Command Prompt, Git Bash and WSL on
Windows; the login shell, bash, zsh and fish elsewhere). SSH terminals from the home screen are local
`ssh` processes in a pseudo console, so password and host-key prompts appear in the tab.
`sshremote terminal [HOST [PORT]]` opens the window on its own; `--split HOST` and `--split-down
HOST` add panes beside or below (`local` is a shell on this computer), as `wt split-pane` does;
`--view text` or `--view markdown` starts in a reading view, and `-- CMD ARGS` runs a command in the
first tab. Input methods (Korean, Japanese, Chinese) compose at the cursor, as in a text field, and
the result is typed when committed. An SSH tab of a host with a saved password answers the password
prompt from the password store (once, so a stale password fails at once).

A remote-screen session's shells run under the agent on a pseudo-terminal (a pseudo console on
Windows) and travel over the same connection, so they need no second login and also work over
`sshremote://`. The window emulates xterm with Alacritty's terminal engine: colours, full-screen
programs such as vim, top and less, wide characters, scrollback (wheel, Shift+PageUp), selection with
copy (Ctrl+C with a selection, otherwise it interrupts) and paste.

- Tabs: double-click to rename, Ctrl+Shift+T new, Ctrl+Tab next.
- Panes: Alt+Shift+= splits the focused pane side by side and Alt+Shift+- one above the other (also
  in the context menu), Alt+Shift+D along the longer side; the new pane runs the same profile. Alt and
  the arrow keys or a click move the focus, which is outlined; only the focused pane takes keys, the
  wheel scrolls the pane under the pointer, and Ctrl+Shift+W closes the focused pane (the tab with
  its last pane).
- Find (Ctrl+Shift+F) searches the scrollback; Ctrl+click opens a link.
- Settings: text size (also Ctrl+= and Ctrl+-), colour schemes (One Half Dark, Campbell, Dracula,
  Solarized Dark, Tango Dark, One Half Light) and copy on select, remembered between runs.
- Three views: the terminal itself, plain text, and Markdown, styled after VS Code's editor and
  Markdown preview (its Dark Modern and Light Modern colours, following the app's theme). The text
  view shows the whole scrollback with line numbers, selectable with the mouse or Ctrl+A. The
  Markdown view draws headings with rules, bold and italic text, links that open on click, lists
  with task boxes, quotes, tables with borders, alignment and a Copy button (tab-separated, for
  spreadsheets), and code blocks with syntax colours and a Copy button. With "Agent session" on, the
  output of Claude Code or Codex is read as a conversation: frames go, box-drawn tables become real
  tables, prompts appear as bubbles, each reply has its own Copy button for its Markdown, and tool
  runs (`Bash(...)`, `Ran ...`) fold up with their output.
- The context menu has copy, paste, select all, find, the reading views and clearing the scrollback.

In a terminal viewer, `Ctrl+] t` swaps the picture for a remote shell that the terminal itself
emulates; `exit` or Ctrl+] returns to the screen. A Windows agent running as the service (SYSTEM)
refuses shells; connect over ssh for one.

## SSH tools

The tools menu on the home screen has:

- SSH keys: the key pairs in `~/.ssh` with a button to copy the public key, and to install it on a
  host (appended once to its `~/.ssh/authorized_keys` in a terminal tab, where the password is asked
  for once); a form creates new Ed25519, RSA 4096 or ECDSA keys with `ssh-keygen`, with an optional
  passphrase.
- SSH hosts: the `Host` entries of `~/.ssh/config` with their address, user, port and key, a form
  that adds one, and a button that opens the file in an editor.
- Saved passwords: SSH passwords per host, kept in Windows Credential Manager, the Secret Service
  (`secret-tool`) on Linux or the macOS keychain, never in sshremote's own files. The remote screen
  and SSH terminal tabs use them to answer the password prompt; only prompts that ask for a password
  or passphrase get one.
- Backup and restore: recent and favorite hosts, settings, `~/.ssh/config` and `known_hosts` in one
  JSON file under Documents/sshremote; with a passphrase the saved passwords go in too, encrypted
  (ChaCha20-Poly1305, key stretched from the passphrase). A copy is made automatically once a day
  (the last 14 are kept). Restoring merges the host lists, adds missing `known_hosts` lines and
  replaces `~/.ssh/config` only when asked, keeping the old one as `config.bak`. Private keys are
  never included.

## Sharing a window or an application

As in a video call (Meet, Loom), the Share button lists what the SSH host can send: its screens,
its open windows, and its applications (all Chrome windows, say). "Active window" and "Active
application" at the top of the lists share the window in front, or every window of its program,
without picking from a list. "Click a window to share" works everywhere without any helper: a hint
appears over the picture, the next click on a window brings it to the front on the SSH host, and that
window is then shared and controlled. The picture is then only that
window or application, so it is smaller and cheaper; anything covering it is greyed out, clicks
outside it are dropped, and keys go only while it has the focus. A chip in the toolbar shows what is
shared and returns to the whole screen when clicked.

On Windows the list comes from the desktop's top-level windows, and the chosen window is brought to
the front. GNOME Shell gives its window list only to its own screen-sharing portal, so on GNOME the
list comes from the sshremote helper, a small GNOME Shell extension (`assets/gnome-helper`) that the
chooser offers to install; GNOME loads it at the next login on the SSH host. The helper is
optional: without it GNOME offers the active window, while the active application and the lists need
it, because only it can tell where a program's other windows are. A window is then
recorded by Mutter on its own (its content shows even when covered), and an application is cut out of
the monitor picture. In a terminal viewer `Ctrl+] v` switches between the focused window and the whole
screen; MCP has `windows` and `share`.

While part of the screen is shared, a click outside it is answered with a notice that says how to get
back to the whole screen, and the terminal viewer's status line shows what is shared. In a terminal
viewer `Ctrl+] v` opens the same chooser as a numbered list over the picture (a number shares a
window, `A` and a number its whole application, `a` and `p` the active window or application, Enter
the whole screen); while something is shared, `Ctrl+] v` goes straight back to the whole screen.

## Viewing modes, screenshots and saving traffic

The View menu fits the SSH host's screen to the window (proportions kept), stretches it to fill the
window, or shows it at a fixed 50-200% of its resolution; a fixed size larger than the window scrolls
when the pointer rests near an edge. Screenshot saves the SSH host's screen as a PNG in Pictures. Pause
stops the picture (input still works) and Update fetches a single frame, so a slow link can be used
picture by picture. In a terminal the menu (Ctrl+]) offers the same: `x` screenshot, `p` pause,
`u` one frame, `o` sound, `c` chat, `g` download a remote path, `l` send a local file, `s` next screen,
`z` zoom; answers and messages appear in the status line.

## Chat

Chat (toolbar, or `Ctrl+] c` in a terminal) opens a small always-on-top window on the SSH host's desktop
for the person sitting there; their replies come back to the viewer. Nothing is stored, and the remote
window closes itself shortly after the viewer leaves.

## Hosts

The home screen lists favorites, recent hosts and the `Host` aliases of `~/.ssh/config` with the
time of the last connection, a search box and a star to pin a host; ssh applies their user, port and
keys as usual. `sshremote --term` offers the same list in a terminal.

## MCP

`sshremote mcp HOST` (or `sshremote mcp sshremote://...`) is a Model Context Protocol server on stdio
that lets an AI agent operate the SSH host, in the manner of Windows-MCP: it works from text and looks
at pictures only when it has to. `sshremote mcp --here` does the same for the computer it runs on,
through a local agent and no ssh; `sshremote mcp --local` is the built-in test desktop.

- `state` lists the window in front as text: its title, the other open windows, and its controls
  numbered as `[id] role "name" =value @x,y` with focused, disabled and checked marks. It comes from
  the accessibility interfaces screen readers use (UI Automation on Windows, AT-SPI on Linux) and
  typically costs a few hundred tokens, where a 1024 px screenshot costs about 800 (image tokens
  depend on the pixel count, not on colours or compression).
- `click_element`, `type_into` and `focus_window` act on those numbers. On Windows a click lands on
  the control's centre with the pointer, or uses the control's own action when it is off screen; text
  goes in through the value interface or the keyboard. On GNOME under Wayland, which does not tell
  where controls are, controls are pressed and filled through AT-SPI's actions and text interfaces.
- `screenshot` (1024 px wide by default, a `region` to zoom in), `click`, `move`, `drag`, `type`,
  `key`, `scroll` and `wait` are for what has no text: pictures, charts, canvas apps and games.
- `monitors`, `select_monitor`, `windows` and `share` choose what is looked at. While a window or a
  program is shared, `state` lists only it, controls outside it are left out, `focus_window` offers
  only that program's windows, and screenshots cover only it, so every call costs fewer tokens and
  nothing outside it is operated. `clipboard_get` and `clipboard_set` reach the remote clipboard.

The picture stream stays paused; coordinates passed to the tools are in the latest screenshot's
pixels and are mapped back to the screen by the server. On GNOME the first `state` switches on the
desktop's accessibility flag (as a screen reader does), which Chrome and Electron apps wait for before
they publish their content. Example client configuration:

```json
{"mcpServers": {
  "remote-pc": {"command": "sshremote", "args": ["mcp", "user@host"]},
  "this-pc": {"command": "sshremote", "args": ["mcp", "--here"]}
}}
```

On GNOME, text the keyboard layout cannot produce (Korean, emoji) is pasted through the clipboard;
the previous clipboard text is put back a second later unless something new was copied.

## Transports

`src/transport.rs` is the only place that knows how a viewer reaches an agent: a local child process,
`ssh -T`, or a direct link. Everything above it sees two byte streams.

Without ssh, `sshremote --serve` waits for viewers on TCP port 7070 and prints a one-time password; a viewer
connects with `sshremote://HOST` and that password (`--password` in a terminal, the password field in the
window). For a host behind NAT, `sshremote --relay` on any reachable machine pairs the two sides by an ID:
the host runs `sshremote --serve --via RELAY` and is reached as `sshremote://ID@RELAY` (port 7071).

Both ends run SPAKE2 on the password, so neither an eavesdropper nor the relay learns anything to
guess offline, and every byte after that is sealed with ChaCha20-Poly1305 under keys derived from the
exchange; a wrong password fails the sealed confirmation at once. Each failed attempt slows the server
down further and ten end it, so a nine-digit password cannot be ground down online. The relay only
copies bytes.

## Use

```sh
sshremote HOST                 # auto preset, under 100 KB/s
sshremote HOST --gui           # the same in a window, up to true colour
sshremote HOST --preset 2      # fixed: full size, 256 colours, 20 fps
sshremote HOST --preset 0      # half size, 1-bit, 5 fps
sshremote HOST --budget 30     # auto, under 30 KB/s
sshremote HOST --mode gray16 --scale 60 --fps 10
sshremote --local              # built-in test desktop, no ssh
sshremote HOST --probe 5 --dump out.ppm   # headless: print traffic, save the reconstructed screen
```

Ctrl+] opens the menu: `q` quit, `0`-`4` presets, `a` auto, `m` colour mode, `+`/`-` scale, `f` fps,
`z` zoom (enlarge a reduced capture to the terminal; off by default inside an ssh session, where the
terminal output itself crosses the network), `t` remote shell, `v` share a window, an application or the whole screen,
`k` Ctrl+Alt+Del, `r` refresh. The status line (window title in `--gui`) shows traffic and round-trip time.

Where the terminal reports key releases (the Windows console, and terminals with the kitty keyboard
protocol: kitty, WezTerm, Ghostty, foot), keys reach the host as separate presses and releases, so
held keys, Shift+click and Ctrl+drag work as in the window; elsewhere each key is a tap with its
modifiers. Pasting into the terminal (bracketed paste) puts the text on the host's clipboard and
presses Ctrl+V there; text copied on the host reaches this computer's clipboard through OSC 52.

Inside tmux the viewer turns on `allow-passthrough` for its pane and sends its images straight to the
outer terminal at the pane's position, because tmux's own sixel support scales images by the cell size
it believes, which is wrong behind ssh clients that report a fixed 640x480 window (Windows OpenSSH).
`Ctrl+] r` repaints after tmux redraws the pane. To watch the screen and work in a shell at the same
time in one terminal, split tmux: `sshremote HOST` in one pane and `ssh HOST` in the other. In the
window, the Terminal button opens shells beside the picture.

| Key | Resolution | Colours | fps |
|---|---|---|---|
| 0 | 50% | black and white | 5 |
| 1 | 75% | 16 greys | 10 |
| 2 | 100% | 256 colours | 20 |
| 3 | 100% | full colour | 30 |
| 4 | 100% | full colour | 60 |

Black and white is made for reading screens: where a pixel's neighbourhood has contrast (text, lines,
icons, field borders) it takes the side of the local mean, so grey text on white and light text on
dark panels both survive, while flat areas keep the plain threshold. A context-modelled coder in the
manner of JBIG2 was measured as well; with 32 px tiles the session's zstd stream, which reuses glyphs
seen anywhere before, stayed 30-50% smaller, so tiles stay bit-packed.

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
