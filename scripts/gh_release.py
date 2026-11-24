"""The release on GitHub, for scripts/release.sh and scripts/publish.sh:

    GITHUB_TOKEN=... python3 scripts/gh_release.py fetch OUT
    GITHUB_TOKEN=... python3 scripts/gh_release.py publish OUT

The release is v and the version in Cargo.toml; its tag must be the commit checked out here.
fetch waits for the draft release GitHub Actions makes for that tag (.github/workflows/release.yml)
and takes the files it built into OUT, each checked against the run's SHA256SUMS and for the build
attestation made with it. publish adds the files built here (`by` other than ci in
release/platforms.json), lists every file's SHA-256 in SHA256SUMS, checks that GitHub holds each
file as it is in OUT, and publishes the release as the latest.
"""

import hashlib
import json
import os
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = os.environ.get("GITHUB_REPOSITORY", "sshremote-dev/sshremote")
API = "https://api.github.com"


def fail(text):
    sys.exit(f"gh_release.py: {text}")


def call(url, method="GET", data=None, accept="application/vnd.github+json", kind=None):
    if not url.startswith("https://"):
        url = API + url
    req = urllib.request.Request(url, data=data, method=method, headers={"Accept": accept, "X-GitHub-Api-Version": "2022-11-28"})
    if kind:
        req.add_header("Content-Type", kind)
    # Not passed on to the storage a download is sent to.
    req.add_unredirected_header("Authorization", "Bearer " + os.environ["GITHUB_TOKEN"])
    with urllib.request.urlopen(req, timeout=600) as r:
        body = r.read()
    if accept != "application/vnd.github+json":
        return body
    return json.loads(body) if body else None


def platforms():
    return json.loads((ROOT / "release" / "platforms.json").read_text(encoding="utf-8"))["platforms"]


def tag():
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines()
    name = "v" + next(line.split('"')[1] for line in cargo if line.startswith("version"))
    head = subprocess.run(["git", "-C", str(ROOT), "rev-parse", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    try:
        ref = call(f"/repos/{REPO}/git/ref/tags/{name}")
    except urllib.error.HTTPError:
        fail(f"{name} is not on GitHub: tag this commit and push the tag first")
    target = ref["object"]["sha"]
    if ref["object"]["type"] == "tag":
        target = call(f"/repos/{REPO}/git/tags/{target}")["object"]["sha"]
    if target != head:
        fail(f"{name} is {target[:12]} on GitHub, not this commit ({head[:12]})")
    return name


def release_of(name):
    return next((r for r in call(f"/repos/{REPO}/releases?per_page=30") if r["tag_name"] == name), None)


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def fetch(out):
    name = tag()
    wanted = [p["file"] for p in platforms() if p["build"]["by"] == "ci"]
    deadline = time.time() + 5400
    while True:
        runs = call(f"/repos/{REPO}/actions/workflows/release.yml/runs?event=push&per_page=10")["workflow_runs"]
        run = next((r for r in runs if r["head_branch"] == name), None)
        if run and run["status"] == "completed" and run["conclusion"] != "success":
            fail(f"the Release run for {name} ended {run['conclusion']}: {run['html_url']}")
        release = release_of(name)
        assets = {a["name"]: a for a in (release or {}).get("assets", [])}
        if release and all(f in assets for f in wanted) and "SHA256SUMS" in assets and run and run["status"] == "completed":
            break
        if time.time() > deadline:
            fail(f"no draft release with {', '.join(wanted)} for {name} within 90 minutes")
        time.sleep(30)
    sums = dict(reversed(line.split()) for line in call(assets["SHA256SUMS"]["url"], accept="application/octet-stream").decode().splitlines() if line.strip())
    for f in wanted:
        data = call(assets[f]["url"], accept="application/octet-stream")
        digest = hashlib.sha256(data).hexdigest()
        if sums.get(f) != digest:
            fail(f"{f} is not the file the run listed in SHA256SUMS")
        try:
            attested = call(f"/repos/{REPO}/attestations/sha256:{digest}")["attestations"]
        except urllib.error.HTTPError:
            attested = []
        if not attested:
            fail(f"{f} (sha256 {digest}) has no build attestation")
        path = Path(out) / f
        path.write_bytes(data)
        path.chmod(0o755)
        print(f"took {f} from {release['html_url']} (sha256 {digest}, attested)")


def upload(release, path, name):
    url = release["upload_url"].split("{")[0] + f"?name={name}"
    return call(url, method="POST", data=Path(path).read_bytes(), kind="application/octet-stream")


def publish(out):
    name = tag()
    release = release_of(name)
    if not release:
        fail(f"no release for {name}: the Release run makes its draft")
    files = [p["file"] for p in platforms()]
    missing = [f for f in files if not (Path(out) / f).is_file()]
    if missing:
        fail(f"{out} lacks {', '.join(missing)}")
    assets = {a["name"]: a for a in release["assets"]}
    for f in files:
        if f not in assets:
            assets[f] = upload(release, Path(out) / f, f)
            print(f"added {f}")
    sums = Path(out) / "SHA256SUMS"
    sums.write_text("".join(f"{sha256(Path(out) / f)}  {f}\n" for f in files))
    if "SHA256SUMS" in assets:
        call(f"/repos/{REPO}/releases/assets/{assets['SHA256SUMS']['id']}", method="DELETE")
    assets["SHA256SUMS"] = upload(release, sums, "SHA256SUMS")
    for f in files + ["SHA256SUMS"]:
        if assets[f].get("digest") != "sha256:" + sha256(Path(out) / f):
            fail(f"GitHub holds another {f} than {out} ({assets[f].get('digest')})")
    done = call(f"/repos/{REPO}/releases/{release['id']}", method="PATCH", data=json.dumps({"draft": False, "make_latest": "true"}).encode(), kind="application/json")
    print(f"published {done['html_url']}")


if __name__ == "__main__":
    if len(sys.argv) != 3 or sys.argv[1] not in ("fetch", "publish"):
        sys.exit(__doc__)
    if "GITHUB_TOKEN" not in os.environ:
        fail("GITHUB_TOKEN is needed")
    Path(sys.argv[2]).mkdir(parents=True, exist_ok=True)
    {"fetch": fetch, "publish": publish}[sys.argv[1]](sys.argv[2])
