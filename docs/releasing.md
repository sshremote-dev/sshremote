# Releasing

A release is one tag and two commands. Everything that ships follows one list,
[`release/platforms.json`](../release/platforms.json): GitHub Actions builds from it, the site's
download section and its lists are written from it, the installers and the program's own update
choose from it, and the package managers' entries are made from it.

## Steps

1. Write what changed in [`release/notes.md`](../release/notes.md), raise `version` in
   `Cargo.toml` (after a dependency changed, run `scripts/licenses.py` for the program's
   `THIRD-PARTY-NOTICES.txt`; CI fails until it is current), commit, then tag and push:

   ```sh
   git tag v0.25.36 && git push origin main v0.25.36
   ```

   [`.github/workflows/release.yml`](../.github/workflows/release.yml) builds every platform marked
   `"by": "ci"` on GitHub's own runners from the tagged source, runs each on its own system
   (`scripts/ci-check.sh`: its version, and a synthetic desktop through its agent), attests it, and
   makes a draft release with the files, `SHA256SUMS` and the notes.

2. Put the release together and publish it:

   ```sh
   GITHUB_TOKEN=... MAC_HOST=mac scripts/release.sh OUT
   GITHUB_TOKEN=... SIGN_HOST=mac FIRST_SIGN_HOST=pc scripts/publish.sh OUT local /var/www/sshremote.dev
   ```

   `release.sh` waits for the draft, takes its files (each checked against the run's `SHA256SUMS`
   and for its attestation) and builds the Mac app on the Mac, where its signing key stays.
   `publish.sh` adds the Mac app to the release, writes `SHA256SUMS` for every file, checks that
   GitHub holds each as built, and publishes the release; then it deploys the site with its signed
   lists, and updates the Homebrew tap (and, with `WINGET_PR=1`, proposes the winget manifests).

## Adding a platform

Add an entry to `release/platforms.json`:

| Field | Means |
|---|---|
| `file` | The file's name in the release |
| `os`, `arch` | Rust's names for the system and processor (`std::env::consts::OS`/`ARCH`): the program's update and the installers pick by these. `universal` takes any processor of that system |
| `label`, `detail` | What the download section shows |
| `build.by` | `ci` (GitHub Actions) or `mac` (built by `scripts/macos.sh` on `$MAC_HOST`) |
| `build.runner`, `build.target`, `build.setup` | The GitHub runner, the Rust target, and which libraries to install first (`linux`, `mingw`, `msvc`) |
| `check` | The runner it runs on before it ships; a file built elsewhere runs there in a job of its own |

That is all: the workflow's matrix, the draft, the site's download list, `download/latest.json`,
the installers, the program's update, the Homebrew tap and the winget manifests take it from there.
Homebrew and winget know a processor by the maps `HOMEBREW_ARCH` and `WINGET_ARCH` in
`scripts/packages.py`; a new system there (FreeBSD, say) needs its own entry kind.

## Adding a package manager

`scripts/packages.py` keeps one function for each package manager in `CHANNELS`. Each takes the
version, `release/platforms.json`'s entries (with each file's SHA-256 and its URL in the GitHub
release) and the release folder, and puts the release where that package manager reads it: the
Homebrew tap is a repository of the organization it commits to; winget's manifests are proposed to
`microsoft/winget-pkgs`. Another, such as Scoop, is one more function in that list.

## Keys and lists

| Key | Where | Signs |
|---|---|---|
| Release key ([`release/keys/release.pub`](../release/keys/release.pub)) | The project's Mac, `~/dev/sign/sshremote_release_ed25519`, never copied | `download/latest.json` (the program's update since 0.25.36, the installers) and `registry/v1/catalog.json` (extensions) |
| First key ([`release/keys/first.pub`](../release/keys/first.pub)) | The Windows PC it was made on | `download/index.json` with its files and `registry/v1/index.json`, which versions up to 0.25.35 read |

The program trusts both keys (`ext::SIGNERS`, kept equal to `release/keys` by a test), so a mirror
of either kind of list keeps working. `scripts/deploy.sh` signs the first key's lists only when
`$FIRST_SIGN_HOST` answers; otherwise those lists and their files stay on the server as they were,
and older versions update the next time it does. Once few enough people run 0.25.35 or older, that
computer is no longer needed for a release.

`download/latest.json` names each file's system, processor, size, SHA-256 and place in the GitHub
release. The files themselves are served by GitHub; the signed list on sshremote.dev is what vouches
for them, so a release is accepted only when both agree.

## Checking a release

- `gh attestation verify FILE -R sshremote-dev/sshremote` checks that a file was built by this
  repository's workflow from its source.
- `sha256sum -c SHA256SUMS` beside the downloaded files.
- `ssh-keygen -Y verify -f signers -I registry@sshremote.dev -n sshremote-release -s latest.json.sig < latest.json`
  with `signers` holding `registry@sshremote.dev namespaces="sshremote-release"` and the release
  key checks the list.
