# sshremote

Remote control over the SSH login you already have: a terminal, the host's screen with keyboard and
mouse, files, and an MCP server for AI agents. Over SSH there is no relay, ID server or open port:
the viewer runs `ssh -T HOST sshremote --agent` and keeps an exact copy of the screen from changed rectangles sent
in one zstd stream (H.264 only for areas that keep moving).

Hosts: Windows (GDI capture, SendInput); GNOME on Wayland (Mutter RemoteDesktop, PipeWire); X11
desktops such as Xfce, MATE, LXQt, Cinnamon, i3, and KDE or GNOME on Xorg (MIT-SHM, XDamage, XFixes,
XTest, with no dialog); and other Wayland desktops through the xdg desktop portal (KDE Plasma,
COSMIC: allowed once at the screen, then kept where the desktop keeps it), with input of its own on
wlroots ones that take none through the portal (Sway, Hyprland: a virtual pointer and keyboard).
A desktop whose screen cannot be shared still gives its terminal and files.

**Cameras** (as RustDesk's view camera): the host's cameras are listed after its monitors, in the
window's monitor menu, the web page's and the terminal viewer's (Ctrl+] n), and shown when picked,
to look at only (Linux through GStreamer, Windows through Media Foundation). A viewer by ID sees
them only when the computer's settings allow it (`remote-access cameras on`, off by default), and
the person at the screen is told each time one is looked at.

Viewers: a native window, or a terminal that shows pictures: sixel (Windows Terminal, foot, VTE)
or, in true colour, the kitty graphics protocol (kitty, Ghostty, WezTerm), found by asking the
terminal (`SSHREMOTE_GRAPHICS=sixel` or `kitty` decides instead). Inside tmux in kitty, kitty images
are placed by Unicode placeholders, so the picture moves and hides with tmux's windows and panes.

**Programs in a pane** (as Remote Desktop's RemoteApp, with the **Programs** extension), opened
three ways: **Search** finds the computer's installed programs by name as the Start menu's search does
(the start of the name or of a word in it, its initials, its program file; its desktop entries on
Linux, the Start menu on Windows, Store apps too), **Command** runs a command line as Run does (a
folder, a file or an address opens with its program), and **File** opens a file or a folder picked
from the computer's folders. What it starts opens in a pane, showing that program alone (with its
menus and dialogs) and taking clicks and keys for it. Home's Tools and Quick open (also when
splitting a pane) open this computer's programs, the program's own window held in the pane (moved
and sized with it, out of the way of the app's menus, hidden with its tab and closed with the pane;
it is unseen as it opens and shown once it is in place, so it never shows outside first): on
Windows, in X11 sessions, and under Wayland, which lets no program place another's windows, on GNOME
through the sshremote helper extension (GNOME loads a new version at the next login), on KDE
Plasma through KWin scripts, and on Sway and Hyprland through their IPC (the program opens on a
workspace out of sight, then floats over its pane without a border). On other Wayland desktops
(COSMIC) and tiling window managers the pane shows it as it would another computer's, or leaves it a
window of its own. A screen's menu opens that computer's programs beside it,
and the web page offers it where its gateway has the extension. Programs are shown alone on Windows,
GNOME (with its helper) and X11 desktops; elsewhere the pane shows the screen. Without the extension
none of these menus are there.

## Build

```sh
cargo build --release
```

Linux needs `libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev libopus-dev libasound2-dev
pkg-config` and a C++ compiler. Put the binary on the host as `~/.local/bin/sshremote` (Windows:
anywhere on `PATH`).

## Tests

`cargo test` runs the tests that need nothing else. The ignored ones start the built program, draw
windows, save passwords or reach other computers. Run them one at a time:
`SSHREMOTE_EXE=$PWD/target/debug/sshremote cargo test -- --ignored --test-threads=1`.

On a Linux computer with GNOME, `scripts/gnome-session.sh up` starts a private headless GNOME
session: its own bus, PipeWire, password store and virtual monitor. Tests run in it with
`scripts/gnome-session.sh run ...` leave the desktop of whoever is signed in alone. `shot FILE`
takes a picture of it through `sshremote mcp`, and `down` stops it.
`SSHREMOTE_SNAP_DIR` keeps the windows' pictures; `secret-tool` must be on `PATH` for the password
tests.

Tests that reach other computers name them by ssh alias:
- `SSHREMOTE_WINDOWS_HOST`: a Windows computer with the service installed.
- `SSHREMOTE_SSH_HOST`: any host with sshremote.
- `SSHREMOTE_GNOME_HOST`: a GNOME host. Add `SSHREMOTE_GNOME_ENV="$(scripts/gnome-session.sh env)"`
  (run there) for its private session.

## The window

`sshremote` opens its home window in two columns:

- **This computer** on the left: how others reach it (its remote access ID, IP addresses, full
  name, and `user@IP` when an SSH server answers), each copied with a click; then remote access by
  ID: its switch, password, state and who is connected, with its settings in a sheet.
- **Going out** on the right: one address field for `user@host`, an alias, an IP or an ID. It says
  what it reads there and opens a terminal, the screen or the files on it; an ID asks for its
  password in place. Under it the computers: this computer first, then favorites and the last
  opened, with the aliases of `~/.ssh/config`. A computer's sheet (Add, or Edit in its menu) keeps
  its name, its address or its `~/.ssh/config` block (host, user, port, key), its saved password,
  its network card for **Wake-on-LAN** and its **tunnels** (`-L`, `-R` or SOCKS through an SSH
  computer; a port here to anything a computer by ID reaches), opened on demand or with the app.
  **On this network** lists the computers nearby with remote access on, to add without typing
  their ID. Computers can carry **tags** (work, servers), and the chips over the list show one
  tag's.
- **Web apps** (with the **Web apps** extension): an `https://` address opens as a pane (**Open in
  a pane**, or + to keep it in the list), and its tools are listed under **Tools** (Google
  Translate, Papago, DeepL, Claude, ChatGPT, Gemini, Gmail, Calendar, Keep, Notion); without it a
  typed address says where to get it. Each is a page
  of Chrome, Edge or Chromium run with no window of its own, shown and driven as a screen is, laid
  out at the pane's size, with back, forward and reload, and the sign-ins of its site kept in a
  profile of its own. Ctrl+V pastes this computer's clipboard there, Ctrl+C copies back.
- The gear opens **Settings**: Appearance, Connections, SSH keys, Passwords, Backup, Extensions,
  Parts and About.

What is opened goes into the workspace:

- **Tabs and panes** as in Windows Terminal: each tab (host and round trip) holds panes split
  beside or below; + opens Quick open, the same address field and computers. The active pane is outlined and the title bar
  follows it. On the right: the pane's **mode** (terminal, screen sharing, files; switching does not
  connect again), its controls, then full screen, split beside, split below, move left and right.
  A narrow window folds the controls into one menu.
- **Full screen**: a pane's own (its dock shows at the top edge; its name switches to the other
  panes) or the whole window as split, the title bar appearing at the top edge.
- **Lost connections** are tried again in the same mode, the shells kept; after 30 s the pane
  offers Reconnect or Close.
- **Projects**: the box beside + keeps the open tabs under a name (each pane's computer and what
  it shows, and how the panes divide each tab; never a password) and opens kept ones again; the
  tabs open when the app closes are kept as **Last session**. Projects show on the home too, and
  Settings, Connections can open the last session with the app. A computer without a saved
  password asks for it, naming the computer, as when opened by hand.
- **Files into a terminal**: files dropped on a terminal, or files and screenshots pasted into it
  (Ctrl+V with a picture on the clipboard), are copied into a folder of their own where its shell
  runs (over SFTP for an `ssh` tab, through the session for a remote screen's terminal; a local
  shell takes its own paths) and their paths are typed at the prompt as a paste, the way an AI
  agent like Claude Code takes a dropped file or screenshot. Progress shows in the pane; what is a
  day old is cleared from that folder as new files come. Files dropped go to the pane they are let
  go over, on Windows, X11 and Wayland alike (under Wayland the app takes drops itself, which
  winit does not).
- **Markdown view**: a terminal's output read as Markdown (its results gathered, live lines left
  out), copied whole or downloaded as a `.md` or `.txt` file, and each code block copied or saved
  as a file of its language.
- Every window draws its own title bar and keeps the system's behaviour: drag and snap,
  double-click, system menu (icon, right click, Alt+Space), resizing edges.
- **Screen sharing**: quality (auto, five presets from black and white at half size and 5 fps to
  full colour at 60, or colours, picture size and frame rate of one's own), the resolution of the
  host's screen itself (its display mode, as RustDesk's: set from the Display menu and set back
  when the connection ends; on Windows, X11, GNOME, KDE Plasma with kscreen-doctor, Sway and
  Hyprland, a virtual screen there taking common sizes), fit, zoom (below 100% the host sends a
  smaller picture), monitors (or
  all of them, a window each, full screen on one monitor each here as Remote Desktop's "use all my
  monitors"), sharing one window or application with its menus and dialogs, special keys, chat,
  a voice call (the person there answers in their window), screenshot, recording to MP4, pause,
  view only, clipboard with files (copied in a file manager here, pasted there, and back), sound,
  and the computer itself: lock it, restart it, keep its own keyboard and mouse out, hide its
  screen from whoever sits there (Windows) and lock it on leaving. F11 and the other keys go to the host;
  Ctrl+Alt+Enter enters and leaves full screen, Ctrl+Alt+Home shows the toolbar there,
  Ctrl+Alt+PgUp/PgDn go to the tab before or after and Ctrl+Alt+Shift+PgUp/PgDn to the pane.
- **Terminal**: tabs and split panes as in Windows Terminal (tabs can be dragged out into new
  windows), profiles for local shells and SSH hosts, find, schemes, mouse reporting for full-screen
  programs. Two reading views:
  - *Text*: the whole output as one document with line numbers, selected as in an editor, typing
    still going to the shell at its cursor;
  - *Markdown*: any output or file shown as styled Markdown (headings, lists, tables, code with
    copy buttons). Box-drawn tables and terminal styles become Markdown, and the screens of
    full-screen programs are stitched into one document as they scroll.
- **Files**: two panes, drag and drop, never overwriting.
- **Tools**: SSH keys, `~/.ssh/config` editor, saved passwords (the system's password store), and
  backups of hosts and settings, with all of `~/.ssh` encrypted under a passphrase (made by hand
  or each day; restored or deleted from the list).
- **Settings** hold every setting once. A pane's quick menu shows the ones that matter there with
  the same controls and links to its page; a screen's menus say "this connection" where they
  change that connection only, and what new ones start with.
- On Windows the window asks for administrator rights at start and works without them. Closed
  while remote access by ID or tunnels are up here, it goes to the notification area: a click
  brings it back, and its menu turns remote access on or off, copies the ID, starts, stops,
  installs or removes the service, and quits.

## In a browser

[web.sshremote.dev](https://web.sshremote.dev/) reaches computers by their ID from any browser,
with nothing to install; it opens nothing else (no SSH, no sign-in: `sshremote web --id-only`).
`sshremote web` serves the same from a computer of your own to a browser, a phone's too:
terminals, screens and files of this computer and its `~/.ssh/config` hosts after its password,
and computers by ID. The screen sends only what changed
(moves for scrolling, lossless WebP for text, JPEG for photographs) at the app's qualities, auto
and black and white included; it has zoom, touch (tap, long press for the right button, two
fingers to scroll and pinch), the keyboard of input methods and phones, keys the browser keeps,
clipboard (text, pictures and files: Ctrl+V on the screen pastes what is copied here, and a
picture copied there comes onto this clipboard), chat, sound and voice calls (WebCodecs), sharing one window or
program, the computer menu (lock, restart, its keyboard kept out, its screen hidden), pause,
lossless screenshots, recording, files dropped on the screen, the terminal below the screen, every
screen of a host in a window of its own, two-step codes, and reconnecting by itself.

### Traffic

Measured in one private GNOME session with the same scripted activity (a document still, typed
into, scrolled, paged and a moving photograph, 30 s each), against RustDesk 1.4.9 (AV1, its
default quality) on a direct connection:

| | sshremote auto (lossless text, 60 fps) | sshremote 256 colours | RustDesk |
|---|---|---|---|
| window opens | 30 KB | 27 KB | 51 KB |
| still | 0.1 KB | 0.1 KB | 0.2 KB |
| typing | 45 KB | 53 KB | 62 KB |
| scrolling | 541 KB | 501 KB | 1052 KB |
| page after page | 221 KB | 187 KB | 440 KB |
| film | 1722 KB | 940 KB | 2492 KB |
| back to text | 38 KB | 8 KB | 46 KB |
| **all** | **2596 KB** | **1717 KB** | **4145 KB** |

The black-and-white preset at half size sends 520 KB for all of it. What makes the difference:
text and drawings go through a palette kept for the session (their bytes repeat from frame to
frame, which the stream's compression finds), scrolls are moves past the scroll bar, only areas
that look like photographs become H.264, and two paints a moment apart go as one.

## Building

`cargo build --release` builds the one program with every part in. Packagers can leave parts out
with `--no-default-features` and a choice of the features `gui`, `remote`, `web` and `mcp` (a
headless server: `--features web,mcp`); what is left out says so when asked for.

## Extensions

Style packs, WASM scripts, command-line programs, resident extensions that add status bar items,
a side panel, palette commands (Ctrl+Shift+P), keys, forms and settings, each limited to the
permissions it names, and tools that open in a pane (web apps). See
[docs/extensions.md](docs/extensions.md).

```sh
sshremote ext available            # from the signed registry
sshremote ext install ID
sshremote ext new my-tool          # a Rust extension to start from
sshremote ext dev my-tool          # build, install and reload on every change
```

## In a terminal

```sh
sshremote HOST                 # the screen in this terminal, auto quality under 100 KB/s
sshremote HOST --preset 0      # 0 black and white 50% 5 fps .. 4 true colour 60 fps
sshremote terminal HOST        # the workspace with a terminal pane; --split HOST adds panes
sshremote --gui HOST --open files
```

`Ctrl+]` opens the menu (quit, presets, colour, zoom, shell, share, Ctrl+Alt+Del). Inside tmux the
images pass through to the outer terminal.

## Windows hosts

The agent starts a helper in the logged-on user's console session. `sshremote --install-service`
(elevated, or from the tools menu) adds a service for the sign-in screen and Ctrl+Alt+Del.

## MCP

`sshremote mcp HOST` (or `mcp --here` for this computer) lets an AI agent read the window in front
as text (accessibility tree) and act on its controls, with screenshots only when needed. Its
`resolution` tool lists the shown screen's resolutions or sets one (set back when it disconnects).

```json
{"mcpServers": {"remote-pc": {"command": "sshremote", "args": ["mcp", "user@host"]}}}
```

## Remote access by ID (without ssh)

For a PC on Wi-Fi or behind NAT with no fixed address: turn on **Remote access by ID** on the left
of the home window. It shows a lasting 9-digit ID and a password (show, copy, a new one); its
settings sheet sets your own password, the relay, a new ID and **Start with the computer**, which
installs the service (Windows, also at the sign-in screen) or a systemd user unit (Linux). On the
other side, type the ID in the address field, give the password and open the screen or files.

```sh
sshremote remote-access            # this computer's ID, password and state
sshremote remote-access on|off|install|uninstall|password [NEW]|relay HOST
sshremote remote-access ask on|off               # the person here lets each viewer in
sshremote remote-access allow keyboard files ... # what viewers may use (all, none)
sshremote remote-access straight|lan|direct on|off  # past the relay; found nearby; straight in nearby
sshremote remote-access service start|stop       # the service, without removing it
sshremote remote-access viewers | end N          # who is connected now; end one
sshremote remote-access passwords lasting|one-time|both  # which passwords viewers may give
sshremote remote-access one-time                 # a new one-time password now
sshremote remote-access requests on|off          # people without the password may ask
sshremote remote-access from 203.0.113.7 10.0.0.0/8   # viewers only from there (or: anywhere)
sshremote sshremote://ID --password ""           # ask to come in, without a password
sshremote sshremote://ID --password P [--code C]
sshremote find                     # computers nearby with remote access on
sshremote wake MAC|ADDRESS         # Wake-on-LAN
```

Viewers give the lasting password, a **one-time password** (as `k7qp-2mz9`, shown beside it), or
either, as chosen in the settings (new installs take both). A one-time password works for one
connection and the minute after it, so a viewer whose network dropped comes back in; then a new
one takes its place and nobody comes back later with what they were told once.

Where the person at the computer allows it, someone with only the ID may **ask to come in**:
they leave the password empty, the person there is asked, and both screens show the same
six-digit code (from the key exchange itself), which someone standing between the two could not
match. One request at a time, and always a click to let them in.

Who comes in and what they may do is the person at the computer's choice: **ask first** (a viewer
with the right password waits for a click there, turned away after a minute), and rights for the
keyboard and mouse, the clipboard, files, sound and calls, terminals, restart and lock, and
tunnels, set in the settings and changed for one session in the window shown there, where the
connection can also be ended. **Two-step sign-in** adds the code of an authenticator app to the
password. Computers on the same network can find this one (its ID, name and network card, never
the password) and, when allowed, come **straight in** on TCP 7073 instead of through the relay,
with the same password check.

With the service installed, an icon starts with each sign-in, in the notification area on
Windows and in the top bar on Linux (GNOME shows it with its AppIndicator extension, which Ubuntu
has on): a click opens the window, and its menu turns remote access on and off, copies the ID,
sets who comes in, shows the window of whoever is connected and starts, stops or removes the
service. That window, on the computer being viewed, stays in the taskbar while someone is
connected, so ending the connection and changing what they may do are always at hand.

On Windows the settings live under `HKLM\SOFTWARE\sshremote`, which only administrators and
SYSTEM can open, so setting it up or seeing the password needs administrator rights (the window
asks for them at start). The relay is its own small program, [sshremote-relay](https://github.com/thinkbigx/sshremote-relay); the one this project runs is `relay.sshremote.dev` (before 0.10 it went by
`sshremote.dev`, which still reaches it and is read as the same relay), reached on TCP and UDP
7071, or on port 80 through a web server when 7071 is shut.

Both ends dial out to the relay, prove the password to each other with SPAKE2 and seal every byte
with ChaCha20-Poly1305; the relay sees neither. Once in, the two look for a **straight path
through their routers** (NAT hole punching, as RustDesk and other remote desktops do): each asks
the relay over UDP which address and port its router shows, they swap those and their own network
addresses, IPv6 ones included, inside the sealed stream, and probe each other's (over IPv6 the
probes open the routers' firewalls the same way; when both work, the same network comes first,
then IPv6, then IPv4). When a path answers, the sealed
stream moves onto it (an ordered, reliable stream over UDP with its own congestion control) with
nothing lost or repeated, and the relay lets the session go; the toolbar's lightning sign shows
it. The session starts on the relay at once and stays there when no path is found (a router that
gives a new port for every destination, on both sides, or a network without UDP); a path that
stops working ends the session, which reconnects through the relay. Either side can turn it off:
the computer in its remote access settings, the viewer in Settings, Connections. IDs are held at the relay by a secret token, wrong
passwords cost more time each, and attempts per address are limited. See
[docs/architecture.md](docs/architecture.md) for how this, SSH and the extensions fit together.

## Credits

Icons: [Phosphor Icons](https://phosphoricons.com) (MIT) via egui-phosphor, see
`assets/fonts/LICENSE-phosphor.txt`.

## Repositories

| Repository | What |
|---|---|
| [sshremote](https://github.com/thinkbigx/sshremote) | The app, the web gateway, the site and `scripts/deploy.sh` |
| [sshremote-relay](https://github.com/thinkbigx/sshremote-relay) | The relay for remote access by ID |
| [sshremote-extensions](https://github.com/thinkbigx/sshremote-extensions) | The registry's extensions; `scripts/deploy.sh` expects it beside this one |
