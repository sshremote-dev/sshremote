- Builds for Windows on Arm and Linux on Arm, each built and run on its own system by GitHub Actions before it ships, with a build attestation like the others (`gh attestation verify FILE -R sshremote-dev/sshremote`).
- Homebrew: `brew install --cask sshremote-dev/tap/sshremote` on macOS (the app, and `sshremote` on `PATH`), `brew install sshremote-dev/tap/sshremote` on Linux.
- The program now updates from a list signed with a new release key, kept on the project's Mac, and takes the file for its system and processor from this GitHub release. Versions up to 0.25.35 keep updating from the list they know.
- Every release is now built and attested by GitHub Actions and published from its draft by scripts that follow one list of platforms (see `docs/releasing.md`).

Install with one line (`irm https://sshremote.dev/install.ps1 | iex` in PowerShell, `curl -fsSL https://sshremote.dev/install.sh | sh` on macOS and Linux), or download the file for your system below. An installed copy updates itself with `sshremote update`. `SHA256SUMS` lists every file's SHA-256, as does the signed [list](https://sshremote.dev/download/latest.json) the program updates from.
