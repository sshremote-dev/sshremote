# Extensions

SSH Remote builds in what every user needs to reach and work on another computer (the workspace,
terminals, SSH tools, settings, and in SSH Remote the remote screen and files; full builds also the
web and MCP gateways) and lets the rest come as extensions, installed from the signed registry at
sshremote.dev or from a file. [architecture.md](architecture.md) draws the line.

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
| Host extension | `programs`, `"activation": "startup"`, `contributes.commands` | Runs beside the app for as long as it is open: status bar items, panels, commands, terminals, events |
| Tools (API 3) | `contributes.panes` | Web apps listed under Tools on the home and in Quick open (`+`); each opens in a pane of its own as a page of Chrome, Edge or Chromium run with no window, its sign-ins kept in a profile of its own per site |
| Programs (API 4) | `contributes.panes` with `app`, `command` or `open` | This computer's own programs under Tools: each is started and its windows shown alone in a pane of its own |
| Menus of the app (API 4) | `contributes.programs`, `contributes.sites` | What the app shows only with such an extension installed: `programs` (with the `programs` permission) the Open a program menus (Home and Quick open, a screen's menu, the web page where its gateway has it, the MCP server's `apps` and `launch`); `sites` any typed `https://` address opening in a pane |

Each of these comes with an extension, not with the app: with none installed there are no Tools,
no Open a program menus and no web pages in panes. The Programs extension declares
`"programs": true` and Web apps `"sites": true` (Web apps 1.0.0, which only lists sites, still
counts for web pages).

A tool in a pane:

```json
{
  "id": "dev.sshremote.web-apps", "name": "Web apps", "version": "1.0.0", "api": 3,
  "contributes": { "panes": [
    { "id": "translate", "title": "Google Translate", "url": "https://translate.google.com",
      "description": "Translate text, documents and pages." } ] }
}
```

It needs no code and no permission: the page runs in the browser's own sandbox, sees nothing of the
app, and gets only what is typed and clicked in its pane. Its `url` must be `https://` (a pane with
another address is left out). Any other site opens the same way by
typing its `https://` address on the home or in Quick open.

A program of this computer's in a pane (API 4) gives, instead of `url`, one of:

| Key | What starts | Permission |
|---|---|---|
| `app` | An installed program by its name as the system's menu shows it (Linux: its desktop entry; Windows: the Start menu, Store apps too), or a list of names, the first installed one taken | none: only what is installed starts, when the person clicks it |
| `command` | A command line, as typed in a terminal | `programs` |
| `open` | A file or an address, opened with the program the system opens it with | `programs` |

```json
{
  "id": "dev.sshremote.programs", "name": "Programs", "version": "1.0.0", "api": 4,
  "contributes": { "panes": [
    { "id": "calculator", "title": "Calculator", "app": ["Calculator", "KCalc", "Galculator"] } ] }
}
```

The program starts on this computer's desktop, as the person using it; its pane then shows its
windows alone (with their menus and dialogs) and takes clicks and keys for it, as sharing one
program does. On a computer reached by SSH or by its ID the same is done from a screen's menu
(Open a program), with that computer's programs; the web page offers it too.

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
| `programs` | A pane that starts a command line (`command`) or opens a file (`open`) on this computer |

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

`packs/ssh-tunnels` in the [extensions repository](https://github.com/thinkbigx/sshremote-extensions) is a complete host extension in about 300 lines of Rust: it reads the
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

