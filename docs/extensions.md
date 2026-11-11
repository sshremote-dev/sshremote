# Extensions

SSH Remote keeps a small core (the terminal workspace, its tabs and splits, the text and Markdown
views, settings) and lets everything else come as an extension, installed from the
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
    "commands": [{ "id": "show", "label": "Show the tunnels", "key": "Ctrl+Alt+T" }],
    "settings": [
      { "id": "interval", "label": "Keep-alive (seconds)", "type": "number", "default": 30 },
      { "id": "token", "label": "API token", "type": "secret" }
    ]
  }
}
```

A command's `key` needs Ctrl or Alt (or is a function key alone), so typing is never taken.
Settings are `text`, `secret`, `bool` or `number`; they are edited on the Extensions page, a
`secret` is kept in the system's password store and the others in the settings file.

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
| `initialize` | `api`, `app`, `platform`, `extension`, `permissions`, `settings` | Right after the program starts |
| `settings` | the settings, by id | One was changed on the Extensions page |
| `command` | `id`, `selection` (with `terminal.selection`) | A contributed command was chosen (palette, status bar, menu) |
| `action` | `panel`, `action`, `args` | A `sshremote:ACTION?KEY=VALUE` link was clicked in one of its panels |
| `event` | `name` (`pane.opened`, `pane.closed`), `data` | With `workspace.read` |
| `shutdown` | | The app is closing, or the extension is being updated; exit soon |

From the extension to the app:

| Method | Params | Permission |
|---|---|---|
| `status.set` | `id`, `text`, `tooltip`, `command` | |
| `status.remove` | `id` | |
| `panel.show` | `id`, `title`, `markdown`, `items` | `items` is a list the app draws as a table under the Markdown: `label`, `detail`, `bold`, `actions` (`title`, `action`, as a `sshremote:` link) |
| `panel.close` | `id` | |
| `notify` | `text` | |
| `input.ask` (a request) | `title`, `prompt`, `value`, `secret` | |
| `input.form` (a request) | `title`, `prompt`, `submit`, `fields`: `id`, `label`, `type` (`text`, `secret`, `number`, `bool`, `choice`), `value`, `choices` | |
| `log` | `text` | |
| `terminal.open` | `argv` (empty: the login shell), `label`, `split` (`right`, `down`, or a new tab) | `workspace.open` |
| `terminal.paste` | `text` | `terminal.write` |

`input.ask` shows a dialog with a field; its answer is `{"value": "..."}`. `input.form` shows one
with several fields; its answer is `{"values": {"port": 5432, "tls": true, ...}}`. Both answer
with an error when the user cancels. What the extension writes to stderr is kept in the log on the Extensions page.

Panels are Markdown: headings, lists, tables, code, and links. A link to
`sshremote:start?host=db` comes back to the extension as the action `start` with `{"host": "db"}`;
other links open in the browser.

## Writing one

`registry/packs/ssh-tunnels` is a complete host extension in about 300 lines of Rust: it reads the
forwards in `~/.ssh/config`, runs `ssh -N` for the ones started from its panel, keeps a count in
the status bar and opens a shell on a host with `terminal.open`.

Start one from the template, a Rust host extension with a status bar item, a command with a key,
a panel, a setting and a form:

```
sshremote ext new my-extension
sshremote ext dev my-extension
```

`ext dev` builds it (`cargo build --release`, when there is a Cargo.toml), installs it, and does
both again each time a file of the folder or of its src/ folder changes. Any language works: name
the program for each platform in `programs`, put it in the folder, and `ext dev` installs it. A
package holds only what the manifest names: extension.json, the programs, the script and layers.

The open app notices the new version within two seconds, stops the old program and starts the new
one. (Packing by hand works too: `sshremote ext pack DIR OUT.sshx`, then `sshremote ext install
OUT.sshx`.) The app runs a copy of the program, in a folder of its own, so the installed files are
never held open; the extension's folder is in `initialize` (`extension.folder`) and in the
`SSHREMOTE_EXTENSION_DIR` environment variable. The Extensions page shows whether it runs,
restarts it, and shows its log.

