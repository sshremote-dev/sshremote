# desk

Remote desktop over plain SSH, viewed inside a terminal. No relay, ID server or open port:
the viewer runs `ssh -T HOST desk --agent`, the agent sends only changed rectangles in one
long-lived zstd stream, and the viewer keeps an exact framebuffer copy and repaints damaged
cells locally with sixel.

## Status

- Agent: GNOME Wayland (Mutter RemoteDesktop/ScreenCast over D-Bus, PipeWire via GStreamer), synthetic test desktop.
- Viewer: sixel terminals (Windows Terminal, foot, WezTerm, VTE with sixel), keyboard and mouse, live presets.
- Planned: Windows (DXGI helper in the console session), macOS (ScreenCaptureKit), kitty graphics,
  GUI window mode, video regions, zero-install terminal mode, scroll detection.

## Build

```sh
cargo build --release
```

On Linux the agent needs `libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev pkg-config`.
Install the binary on the remote host as `~/.local/bin/desk`.

## Use

```sh
desk HOST                 # preset 2: full size, 256 colours, 20 fps
desk HOST --preset 0      # half size, 1-bit, 5 fps
desk HOST --mode gray16 --scale 60 --fps 10
desk --local              # built-in test desktop, no ssh
desk HOST --probe 5 --dump out.ppm   # headless: print traffic, save the reconstructed screen
```

Ctrl+] opens the menu: `q` quit, `0`-`4` presets, `m` colour mode, `+`/`-` scale, `f` fps, `r` refresh.

| Preset | Scale | Colour | fps |
|---|---|---|---|
| 0 glance | 50% | mono | 5 |
| 1 lean | 75% | gray16 | 10 |
| 2 balanced | 100% | 256c | 20 |
| 3 crisp | 100% | true | 30 |
| 4 exact | 100% | true | 60 |
