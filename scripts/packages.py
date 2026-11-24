"""The package managers' entries for the release published from OUT (scripts/publish.sh):

    GITHUB_TOKEN=... python3 scripts/packages.py OUT

Homebrew: the tap sshremote-dev/homebrew-tap (made the first time) gets Casks/sshremote.rb, the
Mac app, and Formula/sshremote.rb, the Linux program, committed and pushed:
    brew install --cask sshremote-dev/tap/sshremote     brew install sshremote-dev/tap/sshremote
winget: the manifests go to OUT/winget/VERSION; with WINGET_PR=1 they are proposed to
microsoft/winget-pkgs from the token's account's fork (`winget install SSHRemote.SSHRemote` once
merged). Both follow release/platforms.json: a new system or processor there comes into them.
"""

import base64
import datetime
import json
import os
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ORG = "sshremote-dev"
RELEASES = f"https://github.com/{ORG}/sshremote/releases"
TAP = f"{ORG}/homebrew-tap"
WINGET_ID = "SSHRemote.SSHRemote"
ABOUT = "One workspace for all your servers and projects: split terminals, Markdown, screens and files over the SSH you already have"
HOMEBREW_ARCH = {"x86_64": "on_intel", "aarch64": "on_arm"}
WINGET_ARCH = {"x86_64": "x64", "aarch64": "arm64"}


def call(url, method="GET", data=None):
    if not url.startswith("https://"):
        url = "https://api.github.com" + url
    body = json.dumps(data).encode() if data is not None else None
    req = urllib.request.Request(url, data=body, method=method, headers={"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"})
    req.add_unredirected_header("Authorization", "Bearer " + os.environ["GITHUB_TOKEN"])
    if body is not None:
        req.add_header("Content-Type", "application/json")
    with urllib.request.urlopen(req, timeout=120) as r:
        text = r.read()
    return json.loads(text) if text else None


def release(out):
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines()
    version = next(line.split('"')[1] for line in cargo if line.startswith("version"))
    sums = dict(reversed(line.split()) for line in (Path(out) / "SHA256SUMS").read_text().splitlines() if line.strip())
    platforms = json.loads((ROOT / "release" / "platforms.json").read_text(encoding="utf-8"))["platforms"]
    for p in platforms:
        p["sha256"] = sums[p["file"]]
        p["url"] = f"{RELEASES}/download/v{version}/{p['file']}"
    return version, platforms


def cask(version, platforms):
    mac = next(p for p in platforms if p["os"] == "macos")
    return f'''cask "sshremote" do
  version "{version}"
  sha256 "{mac['sha256']}"

  url "{RELEASES}/download/v#{{version}}/{mac['file']}"
  name "SSH Remote"
  desc "{ABOUT.split(':')[0]}"
  homepage "https://sshremote.dev/"

  depends_on macos: ">= :monterey"

  app "SSH Remote.app"
  binary "#{{appdir}}/SSH Remote.app/Contents/MacOS/sshremote"

  zap trash: "~/.config/sshremote"

  caveats <<~EOS
    SSH Remote is not notarized by Apple. The first time, open it with a right click and Open, or
    allow it under System Settings > Privacy & Security. `curl -fsSL https://sshremote.dev/install.sh | sh`
    installs it without that step.
  EOS
end
'''


def formula(version, platforms):
    arches = "\n".join(
        f'''    {HOMEBREW_ARCH[p["arch"]]} do
      url "{p['url']}"
      sha256 "{p['sha256']}"
    end'''
        for p in platforms
        if p["os"] == "linux"
    )
    return f'''class Sshremote < Formula
  desc "{ABOUT.split(':')[0]}"
  homepage "https://sshremote.dev/"
  version "{version}"
  license "MIT"

  depends_on :linux

  on_linux do
{arches}
  end

  def install
    bin.install Dir["sshremote-linux-*"].first => "sshremote"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/sshremote --version")
  end
end
'''


def git(*args, cwd=None):
    auth = base64.b64encode(f"x-access-token:{os.environ['GITHUB_TOKEN']}".encode()).decode()
    subprocess.run(["git", "-c", f"http.extraHeader=Authorization: Basic {auth}", *args], cwd=cwd, check=True)


def homebrew(version, platforms):
    try:
        call(f"/repos/{TAP}")
    except urllib.error.HTTPError as e:
        if e.code != 404:
            raise
        call(f"/orgs/{ORG}/repos", "POST", {"name": "homebrew-tap", "description": "Homebrew tap for SSH Remote", "homepage": "https://sshremote.dev", "has_wiki": False, "has_projects": False, "auto_init": True})
        time.sleep(3)
    with tempfile.TemporaryDirectory() as tmp:
        git("clone", "-q", f"https://github.com/{TAP}.git", tmp)
        tap = Path(tmp)
        (tap / "Casks").mkdir(exist_ok=True)
        (tap / "Formula").mkdir(exist_ok=True)
        (tap / "Casks" / "sshremote.rb").write_text(cask(version, platforms))
        (tap / "Formula" / "sshremote.rb").write_text(formula(version, platforms))
        (tap / "README.md").write_text(
            "# Homebrew tap for SSH Remote\n\n"
            f"{ABOUT}. See [sshremote.dev](https://sshremote.dev).\n\n"
            "```sh\nbrew install --cask sshremote-dev/tap/sshremote   # macOS: the app, and sshremote on PATH\n"
            "brew install sshremote-dev/tap/sshremote          # Linux\n```\n\n"
            "Written by `scripts/packages.py` of [sshremote](https://github.com/sshremote-dev/sshremote) with each release.\n"
        )
        subprocess.run(["git", "add", "-A"], cwd=tmp, check=True)
        if subprocess.run(["git", "diff", "--cached", "--quiet"], cwd=tmp).returncode == 0:
            print(f"homebrew: {TAP} already has {version}")
            return
        subprocess.run(["git", "commit", "-q", "-m", f"chore: update SSH Remote to {version}"], cwd=tmp, check=True)
        git("push", "-q", "origin", "HEAD", cwd=tmp)
        print(f"homebrew: {TAP} has {version}")


def winget_manifests(version, platforms, folder):
    folder.mkdir(parents=True, exist_ok=True)
    head = f"PackageIdentifier: {WINGET_ID}\nPackageVersion: {version}\n"
    tail = "ManifestVersion: 1.10.0\n"
    schema = "# yaml-language-server: $schema=https://aka.ms/winget-manifest.{}.1.10.0.schema.json\n"
    installers = "".join(
        f"- Architecture: {WINGET_ARCH[p['arch']]}\n  InstallerUrl: {p['url']}\n  InstallerSha256: {p['sha256'].upper()}\n"
        for p in platforms
        if p["os"] == "windows"
    )
    files = {
        f"{WINGET_ID}.yaml": schema.format("version") + head + "DefaultLocale: en-US\nManifestType: version\n" + tail,
        f"{WINGET_ID}.installer.yaml": schema.format("installer")
        + head
        + "InstallerType: portable\nCommands:\n- sshremote\n"
        + f"ReleaseDate: {datetime.date.today().isoformat()}\nInstallers:\n{installers}ManifestType: installer\n"
        + tail,
        f"{WINGET_ID}.locale.en-US.yaml": schema.format("defaultLocale")
        + head
        + "PackageLocale: en-US\nPublisher: SSH Remote\nPublisherUrl: https://sshremote.dev/\n"
        + f"PublisherSupportUrl: https://github.com/{ORG}/sshremote/issues\nPackageName: SSH Remote\nPackageUrl: https://sshremote.dev/\n"
        + f"License: MIT\nLicenseUrl: https://github.com/{ORG}/sshremote/blob/main/LICENSE\n"
        # Quoted: the words hold ": ", which YAML would read as a key.
        + f"ShortDescription: {json.dumps(ABOUT + '.')}\nMoniker: sshremote\n"
        + "Tags:\n- ssh\n- terminal\n- remote-desktop\n- sftp\n- developer-tools\n- mcp\n"
        + f"ReleaseNotesUrl: {RELEASES}/tag/v{version}\nManifestType: defaultLocale\n"
        + tail,
    }
    for name, text in files.items():
        (folder / name).write_text(text)
    return files


def winget_pr(version, files):
    upstream = "microsoft/winget-pkgs"
    me = call("/user")["login"]
    try:
        call(f"/repos/{me}/winget-pkgs")
    except urllib.error.HTTPError:
        call(f"/repos/{upstream}/forks", "POST", {})
        for _ in range(60):
            time.sleep(5)
            try:
                call(f"/repos/{me}/winget-pkgs")
                break
            except urllib.error.HTTPError:
                pass
    call(f"/repos/{me}/winget-pkgs/merge-upstream", "POST", {"branch": "master"})
    base = call(f"/repos/{upstream}/git/ref/heads/master")["object"]["sha"]
    branch = f"sshremote-{version}"
    call(f"/repos/{me}/winget-pkgs/git/refs", "POST", {"ref": f"refs/heads/{branch}", "sha": base})
    path = f"manifests/s/{WINGET_ID.replace('.', '/')}/{version}"
    try:
        call(f"/repos/{upstream}/contents/manifests/s/{WINGET_ID.replace('.', '/')}")
        kind = "New version"
    except urllib.error.HTTPError:
        kind = "New package"
    title = f"{kind}: {WINGET_ID} version {version}"
    for name, text in files.items():
        call(f"/repos/{me}/winget-pkgs/contents/{path}/{name}", "PUT", {"message": title, "content": base64.b64encode(text.encode()).decode(), "branch": branch})
    pr = call(f"/repos/{upstream}/pulls", "POST", {"title": title, "head": f"{me}:{branch}", "base": "master", "body": f"{ABOUT}.\n\nRelease: {RELEASES}/tag/v{version}"})
    print(f"winget: {pr['html_url']}")


def main():
    if len(sys.argv) != 2:
        sys.exit(__doc__)
    out = Path(sys.argv[1])
    version, platforms = release(out)
    homebrew(version, platforms)
    files = winget_manifests(version, platforms, out / "winget" / version)
    print(f"winget: manifests in {out / 'winget' / version}")
    if os.environ.get("WINGET_PR") == "1":
        winget_pr(version, files)


if __name__ == "__main__":
    main()
