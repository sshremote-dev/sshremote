# sshremote

Remote control over the SSH login you already have: a terminal, the host's screen with keyboard and
mouse, files, and an MCP server for AI agents. Over SSH there is no relay, ID server or open port:
the viewer runs `ssh -T HOST sshremote --agent` and keeps an exact copy of the screen from changed rectangles sent
in one zstd stream (H.264 only for areas that keep moving).

Hosts: Windows (GDI capture, SendInput) and GNOME on Wayland (Mutter RemoteDesktop, PipeWire).
Viewers: a native window, or a sixel terminal (Windows Terminal, WezTerm, foot, VTE).

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

`sshremote` opens a small open window: a new connection, this computer (local shells, its screen
and files), the SSH hosts (favorites, recent, `~/.ssh/config`) and the tools.
What is opened goes into the workspace:

- **Tabs and panes** as in Windows Terminal: each tab (host and round trip) holds panes split
  beside or below; + opens the same open dialog. The active pane is outlined and the title bar
  follows it. On the right: the pane's **mode** (terminal, screen sharing, files; switching does not
  connect again), its controls, then full screen, split beside, split below, move left and right.
  A narrow window folds the controls into one menu.
- **Full screen**: a pane's own (its dock shows at the top edge; its name switches to the other
  panes) or the whole window as split, the title bar appearing at the top edge.
- **Lost connections** are tried again in the same mode, the shells kept; after 30 s the pane
  offers Reconnect or Close.
- Every window draws its own title bar and keeps the system's behaviour: drag and snap,
  double-click, system menu (icon, right click, Alt+Space), resizing edges.
- **Screen sharing**: quality, fit, monitors, sharing one window or application, special keys,
  chat, screenshot, pause, view only, clipboard, sound. F11 and the other keys go to the host;
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
  backups of hosts and settings, with all of `~/.ssh` encrypted under a passphrase.
- On Windows the window asks for administrator rights at start and works without them.

## Editions

- **SSH Remote** (`--no-default-features --features gui,remote`): everything above built in, except
  the web gateway and the MCP server, which come as extensions.
- **SSH Remote Terminal** (`--no-default-features --features gui`): the terminal and tools; screens,
  files, chat and sound come as the Remote desktop extension.
- A plain `cargo build` builds everything in (`gui`, `remote`, `web`, `mcp`).

## Extensions

Style packs, WASM scripts, command-line programs and resident extensions that add status bar items,
a side panel, palette commands (Ctrl+Shift+P), keys, forms and settings, each limited to the
permissions it names. See [docs/extensions.md](docs/extensions.md).

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
as text (accessibility tree) and act on its controls, with screenshots only when needed.

```json
{"mcpServers": {"remote-pc": {"command": "sshremote", "args": ["mcp", "user@host"]}}}
```

## Remote access by ID (without ssh)

For a PC on Wi-Fi or behind NAT with no fixed address: open dialog → This computer → **Remote
access by ID**. Turn it on; the card shows a lasting 9-digit ID and a password (show, copy, a new
one, or your own). **Start with the computer** installs the service (Windows, also at the sign-in
screen) or a systemd user unit (Linux). On the other side, type the ID in New connection, give the
password and open the screen or files.

```sh
sshremote remote-access            # this computer's ID, password and state
sshremote remote-access on|off|install|uninstall|password [NEW]|relay HOST
sshremote sshremote://ID@sshremote.dev --password P
```

On Windows the settings live under `HKLM\SOFTWARE\sshremote`, which only administrators and
SYSTEM can open, so setting it up or seeing the password needs administrator rights (the window
asks for them at start). The relay is its own small program, [sshremote-relay](https://github.com/thinkbigx/sshremote-relay); it is reached on TCP 7071, or on port 80 through a web server when
7071 is shut.

Both ends dial out to the relay, prove the password to each other with SPAKE2 and seal every byte
with ChaCha20-Poly1305; the relay sees neither. IDs are held at the relay by a secret token, wrong
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
