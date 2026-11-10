# Extensions

SSH Remote keeps a small core (the terminal workspace, its tabs and splits, the text and Markdown
views, projects, settings) and lets everything else come as an extension, installed from the
signed registry at sshremote.dev or from a file.

## Design

The model follows the editors and terminals that have the richest extension ecosystems, adapted to
an app that must stay safe on machines that hold SSH keys:

| Idea | Taken from | Here |
|---|---|---|
| Extensions run in their own process and talk with the app over a protocol | VS Code's extension host, the Language Server Protocol, MCP | JSON-RPC 2.0 over the program's stdin and stdout, one message per line. An extension can be written in any language. A crash cannot take the app down. |
| What an extension adds is declared in its manifest | VS Code `contributes`, Zed `extension.toml` | `extension.json`: commands, background layers, programs, permissions |
| Small, sandboxed code for simple jobs | Zed and Lapce (WebAssembly) | Script extensions: WebAssembly run with a fuel and memory limit, no files or network |
| Explicit permissions | Browser extensions, Deno | Each request is checked against the permissions in the manifest; the Extensions page shows them |
| Signed, versioned packages with roll-back | Package managers | `.sshx` packages listed in a signed index, checked by SHA-256, one package per platform for programs |

## Kinds of extension

| Kind | Manifest | What it can do |
|---|---|---|
| Style pack | `contributes.layers` | Pictures and Lottie animations behind the terminals |
| Script | `main` (a `.wasm` file), `contributes.commands` | Commands in the terminal's right-click menu and the command palette that change the selected text |
| Program | `programs`, `contributes.provides` | Commands of the `sshremote` command line (`sshremote web`, `sshremote mcp`) |
| Host extension | `programs`, `"activation": "startup"`, `contributes.commands` | Runs beside the app for as long as it is open: status bar items, panels, commands, terminals, events |

## The manifest

```json
{
  "id": "dev.example.tunnels",
  "name": "SSH tunnels",
  "version": "1.0.0",
  "api": 2,
  "description": "What it does, in a sentence.",
  "activation": "startup",
  "permissions": ["native", "workspace.open"],
  "programs": {
    "windows-x86_64": "tunnels.exe",
    "linux-x86_64": "tunnels"
  },
  "contributes": {
    "commands": [{ "id": "show", "label": "Show the tunnels" }]
  }
}
```

Permissions:

| Permission | Allows |
|---|---|
| `native` | Running its own program (needed by every program and host extension) |
| `workspace.open` | `terminal.open`: opening terminals in the workspace |
| `workspace.read` | Receiving `event` notifications about the panes opened and closed |
| `terminal.selection` | Receiving the selected terminal text with a command |
| `terminal.write` | `terminal.paste`: typing text into the active terminal, as a paste |

## The protocol (API 2)

Each message is a JSON-RPC 2.0 object on one line. A message with an `id` is a request and gets an
answer; one without is a notification.

From the app to the extension:

| Method | Params | When |
|---|---|---|
| `initialize` | `api`, `app`, `platform`, `extension`, `permissions` | Right after the program starts |
| `command` | `id`, `selection` (with `terminal.selection`) | A contributed command was chosen (palette, status bar, menu) |
| `action` | `panel`, `action`, `args` | A `sshremote:ACTION?KEY=VALUE` link was clicked in one of its panels |
| `event` | `name` (`pane.opened`, `pane.closed`), `data` | With `workspace.read` |
| `shutdown` | | The app is closing, or the extension is being updated; exit soon |

From the extension to the app:

| Method | Params | Permission |
|---|---|---|
| `status.set` | `id`, `text`, `tooltip`, `command` | |
| `status.remove` | `id` | |
| `panel.show` | `id`, `title`, `markdown` | |
| `panel.close` | `id` | |
| `notify` | `text` | |
| `log` | `text` | |
| `terminal.open` | `argv` (empty: the login shell), `label`, `split` (`right`, `down`, or a new tab) | `workspace.open` |
| `terminal.paste` | `text` | `terminal.write` |

What the extension writes to stderr is kept in the log on the Extensions page.

Panels are Markdown: headings, lists, tables, code, and links. A link to
`sshremote:start?host=db` comes back to the extension as the action `start` with `{"host": "db"}`;
other links open in the browser.

## Writing one

`registry/packs/ssh-tunnels` is a complete host extension in about 300 lines of Rust: it reads the
forwards in `~/.ssh/config`, runs `ssh -N` for the ones started from its panel, keeps a count in
the status bar and opens a shell on a host with `terminal.open`.

To try an extension while writing it, pack the folder and install the file:

```
sshremote ext pack my-extension my-extension.sshx
sshremote ext install my-extension.sshx
```

Installing again stops the running program, puts the new one in place and starts it. The
Extensions page shows whether it runs, restarts it, and shows its log.

## Projects

The core keeps the open tabs as the last session, reopened at start, and as named projects (the
Projects page of the open view, `sshremote --project NAME`, or the command palette). A project
holds the tabs, the splits, what each pane runs and the view it is shown in, and the host, port
and options of each connection. It never holds a password: a host that refuses the login asks with
a dialog that names the host, the user, the port and what the connection is for.
