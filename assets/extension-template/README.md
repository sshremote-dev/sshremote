# EXT_NAME

An extension for SSH Remote (sshremote.dev), made with `sshremote ext new`.

    sshremote ext dev .

builds it (`cargo build --release`), installs it, and installs it again each time a file changes;
an open SSH Remote window runs the new version within two seconds. Its status bar item, its
commands (Ctrl+Shift+P) and its panel show up there; what it writes to stderr is on the
Extensions page.

The protocol is described in docs/extensions.md of SSH Remote: one JSON-RPC message per line on
stdin and stdout.
