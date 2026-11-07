# sshremote

Remote control over the SSH login you already have: a terminal, the host's screen with keyboard and
mouse, files, and an MCP server for AI agents. No relay, ID server or open port: the viewer runs
`ssh -T HOST sshremote --agent` and keeps an exact copy of the screen from changed rectangles sent
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

## The window

`sshremote` opens the home screen: a host field and the connections (favorites, recent hosts,
`~/.ssh/config`). Each connection opens a **terminal**, the **remote screen** or the **files**.

- Every window draws its own title bar (tabs or toolbar beside the window buttons) and keeps the
  system's behaviour: drag and snap, double-click, system menu (icon, right click, Alt+Space),
  resizing edges.
- **Remote screen**: toolbar in the title bar (in full screen, a dock that hides at the top edge):
  quality, fit, monitors, sharing one window or application, special keys, files, terminal (in place
  of the picture), chat, screenshot, pause, view only, clipboard, sound. F11 and the other keys go to the host;
  Ctrl+Alt+Enter (or the toolbar) enters and leaves full screen.
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

## In a terminal

```sh
sshremote HOST                 # the screen in this terminal, auto quality under 100 KB/s
sshremote HOST --preset 0      # 0 black and white 50% 5 fps .. 4 true colour 60 fps
sshremote terminal HOST        # the terminal window; --split HOST adds panes
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

## Without ssh

`sshremote --serve` waits for viewers with a one-time password (SPAKE2, ChaCha20-Poly1305);
`sshremote --relay` pairs hosts and viewers behind NAT.

## Credits

Icons: [Phosphor Icons](https://phosphoricons.com) (MIT) via egui-phosphor, see
`assets/fonts/LICENSE-phosphor.txt`.
