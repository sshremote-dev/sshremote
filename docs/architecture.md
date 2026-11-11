# What is built in and what is an extension

One rule decides it: **what every user needs to reach and work on another computer is built in;
what only some users need, or what can change without the app, is an extension.**

| Layer | Contents | Where it lives |
|---|---|---|
| Core | Workspace (tabs, panes, windows), terminal and local shells, SSH hosts and `~/.ssh` tools, saved passwords, the open dialog, look and themes, the extension host | Built into both editions |
| Remote | Agent, screen sharing, files, chat, sound, terminal viewer (`sshremote HOST`), remote access by ID through a relay, the Windows service | Built into **SSH Remote**; the **Remote desktop** extension for **SSH Remote Terminal** |
| Gateways | Web gateway (`sshremote web`), MCP server (`sshremote mcp`) | Built into full builds with `web`/`mcp`; extensions otherwise |
| Add-ons | Style packs, WASM scripts, command-line programs, resident extensions (status bar, panels, palette commands, keys, forms, settings) | Always extensions, from the signed registry or a `.sshx` file |

## Why this split

- **Connecting is never an add-on.** Without SSH, terminals and the open dialog there is nothing to
  extend, so they stay in the core and work offline, with no registry.
- **Remote is one unit.** The agent, the viewer, files, the service and remote access by ID share
  one protocol (`src/proto.rs`) and must match in version, so they move together: built in, or the
  one Remote desktop extension. A computer reached by ID runs the same agent as one reached by SSH.
- **Gateways are servers.** They open ports or serve other programs, so a user adds them on purpose.
- **Extensions cannot break the core.** They run as their own processes (or in a WASM sandbox),
  talk JSON-RPC to the extension host, and may do only what their manifest's permissions name.
  A failing extension is stopped and reported; the app goes on.

## How the two ways to connect relate

| | SSH | Remote access by ID |
|---|---|---|
| For | One host you reach directly (server, VM, LAN, VPN) | A PC on Wi-Fi or behind NAT, with no fixed address or SSH server |
| Sign-in | Your SSH keys or password | The ID and the password set on that computer |
| Path | `ssh -T HOST sshremote --agent` | Both ends dial out to a relay (TCP 7071), which only copies sealed bytes |
| Opens | Terminal, screen, files | Screen and files |
| Unattended | sshd runs anyway | The service (Windows) or a systemd user unit starts it with the computer |

Both end in the same agent and the same viewer, so every screen feature works on both.
