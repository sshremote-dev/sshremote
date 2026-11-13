# SSH Remote: what it does, and how it is built

One app reaches other computers (by SSH or by a remote access ID) as terminals, screens and
files, and lets other people reach this one. Everything a user needs for that is built in;
extensions only add looks, commands and panels.

## What it does

| Group | Features |
|---|---|
| Going out | SSH host (alias, `user@host[:port]`, IP) as terminal, screen or files; a computer by its ID and password as screen or files; this computer's own terminal, screen and files |
| Coming in | Remote access by ID: on and off, password, two-step code, relay, start with the computer (Windows service); asking the person here first, and the rights viewers have (changed live in the window shown here); found on this network and reached straight; moved past the relay onto a straight path through both routers (`udp`, NAT hole punching); who is connected now, and ending it; the addresses others reach this computer at (ID, IP, host name) |
| Computers | One list: favorites, recent, `~/.ssh/config` aliases, computers by ID, this computer; per computer its user, port, key, saved password and tunnels |
| Tunnels | Local, remote and SOCKS port forwards through an SSH computer, started by hand or with the app |
| Workspace | Tabs and split panes, each pane a terminal, screen or files on one connection; full screen; reconnecting; chat, voice calls, sound, clipboard with files, recording, sharing one window or program; the computer itself (lock, restart, its input kept out, its screen hidden) |
| Settings | Appearance, SSH keys, saved passwords, backup, extensions, parts of the app on or off, about |
| Servers and command line | Web gateway (`sshremote web`), MCP server (`sshremote mcp`), terminal viewer (`sshremote HOST`), `remote-access`, `ext`, `module` |

## One app

There is one edition. The remote desktop, the web gateway and the MCP server are part of it, so no
extension provides them and no second program has to be fetched for them. A part a user does not
want is turned off in Settings > Parts (`sshremote module off ID`).

Extensions are add-ons only: style packs (background layers), WASM scripts and commands (text
tools), and resident extensions (status bar, panels, palette commands, forms). They run as their own
processes or in a WASM sandbox and may do only what their manifest's permissions name.

Port forwarding is SSH's own and is built in, per computer.

## The code

| Layer | Modules | |
|---|---|---|
| Engine | `proto`, `net`, `udp`, `transport`, `session`, `agent`, `capture/*`, `video`, `audio`, `files`, `clip`, `chat`, `pty`, `conpty`, `keys`, `xk`, `keygrab`, `access`, `relay`, `service`, `admin`, `console_user` | kept |
| Coming in | `grant` (rights), `hostctl` (the computer itself), `porttunnel` (tunnels by ID), `lan` (found nearby, Wake-on-LAN), `totp` (two-step codes), `record` (MP4) | new |
| Data | `computers` (the one list), `tunnels`, `vault`, `hosts` (settings and `~/.ssh/config`), `backup` | `computers` and `tunnels` new |
| Servers | `web`, `mcp` | kept |
| Window | `gui/mod.rs` (app, windows), `gui/home/*` (this computer, connect, computer sheet, quick open), `gui/settings/*`, `gui/workspace/*`, `gui/remote/*`, `termwin/*`, `filewin` | home and settings new |
| Extensions | `ext` (manifests, registry, install), `exthost` (resident), `script` (WASM) | without providers |

## The home window

Two columns, each fact shown once.

**Left: this computer, as others reach it.** Its name; its addresses, each with a copy button: the
remote access ID, its IP addresses, its host name, and `user@IP` when an SSH server answers here;
the remote access switch and password (show, copy, new); who is connected, each with Disconnect;
remote access settings (relay, start with the computer) in a sheet of their own.

**Right: going out.** One address field takes anything: an alias, `user@host[:port]`, an IP, or a
nine-digit ID. Under it, what was recognised ("SSH host", "Computer by ID", "This computer") and
the actions it allows: Terminal, Screen, Files (an ID has no terminal). A password field appears in
the same card only when the address needs one: an ID, or an SSH computer that refused the keys (or
when asked for with the key button). There are no fields inside drop-downs.

Below, the computers: this computer first, then favorites, then the rest by last use; computers of
`~/.ssh/config` among them. A row shows the name, its address, how it is reached and when it was
last used; its actions appear on hover and by keyboard. Its sheet (Edit) holds the name, user,
port, key, saved password and tunnels; an SSH alias is written back to `~/.ssh/config`.

The gear opens Settings: Appearance, Connections, SSH keys, Passwords, Backup, Extensions, Parts, About.

In a workspace, `+` opens Quick open: the same field and computers, filtered as one types; the
arrow keys pick a computer and Enter opens it with the last action chosen.

## The computers list

```
Computer { address, port, name, favorite, last_used, in_ssh_config, tunnels: [Tunnel] }
Tunnel   { kind: Local | Remote | Socks, port, dest, start_with_app }
```

The address is how it is reached and what it is known by: an alias of `~/.ssh/config` (whose host,
user, port and key stay there), `user@host`, or `sshremote://ID@RELAY`. It is kept in
`computers.json` in the settings folder; `~/.ssh/config` aliases are read live and merged by
address. The older `recent.txt`, `favorites.txt` and `seen.txt` are read once into it.
Passwords stay in the system's password store, keyed by the computer's address. The window, the
web gateway and the terminal picker all read this one list.

## Straight paths by ID (NAT hole punching)

A session by ID starts on the relay. After the password check an agent that may go straight
(`remote-access straight`, on by default) sends one empty sealed frame, which older viewers pass
over. A viewer that may too then sends control frames: sealed like the stream's, in the same
order, with the top bit of the length set, and sent only to a side known to read them.

1. Each side binds a UDP socket for IPv4 and, where it has IPv6, one for IPv6, and asks the
   relay, on UDP at the relay's port, where it sees the IPv4 one from (`sshremote-addr 1 NONCE`,
   answered with the address and port; the relay answers 20 a second per address, never more
   bytes than asked).
2. `offer TAG FIRST ADDRESS...` (viewer) and `answer ADDRESS...` (agent): the router's address
   and the computer's own, IPv6 ones included (no router changes those; the one it sends from to
   the internet first, a temporary one where those are used), and an 8-byte tag for the UDP
   packets.
3. Both probe all of the other's addresses (and any address a probe came from). The side named
   FIRST probes at once, the other a second later or when it first hears a probe, so a router
   never sees an answer before its own side sent: a Linux router answering a stray keeps it as a
   flow and moves its side to another port, and some routers drop the mapping. IPv6 needs the
   same: its routers have no NAT but a firewall that lets in only what its side began. The viewer
   names the path (one on a private network if one answered in 100 ms, else IPv6, else IPv4) and
   the agent takes it. Six seconds a try, each on new ports: the viewer first, then the agent,
   then both, whose IPv4 probes die two hops out for 0.6 s (past their own router, not as far as
   the other's) before going all the way.
4. Each side then writes `move` on the relay and its frames from there on onto the UDP stream; a
   reader that reads `move` reads on from the UDP stream. Once both moved the relay's connection
   is shut.

The UDP stream (`udp`) numbers packets of at most 1180 bytes and acknowledges ranges: losses are
found by later packets arriving (by time and by count) and by timeouts, a lone tail packet is sent
again early, the window halves on loss and grows as TCP's, and the receiver's room limits the
sender. Every packet carries the tag; twenty seconds of nothing ends it, and a viewer whose
straight path was lost uses the relay for that ID for ten minutes. The web gateway, beside the
relay, does not look for one.

## Order of work

1. One edition: remove the provider plumbing, the Terminal edition's branches and the three packs
   that duplicate built-in parts.
2. The computers list and tunnels as data.
3. The home window, Settings and Quick open.
4. Tunnels in the window.
5. Site, registry, release scripts and README for one download.
6. Tests in the private GNOME session and on Windows, then a release.
