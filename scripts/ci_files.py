"""Takes a release's Linux and Windows files from the GitHub Actions run that built them for
COMMIT's tag (.github/workflows/release.yml), waiting for it to finish, into OUT:

    GITHUB_TOKEN=... python3 scripts/ci_files.py COMMIT OUT

Each file must carry the build attestation that run made for it.
"""

import hashlib
import io
import json
import os
import sys
import time
import urllib.error
import urllib.request
import zipfile

REPO = os.environ.get("GITHUB_REPOSITORY", "sshremote-dev/sshremote")
FILES = ["sshremote-linux-x86_64", "sshremote-windows-x86_64.exe"]


def get(url, raw=False):
    if not url.startswith("https://"):
        url = "https://api.github.com" + url
    req = urllib.request.Request(url, headers={"Accept": "application/vnd.github+json", "X-GitHub-Api-Version": "2022-11-28"})
    # Not passed on to the storage an artifact's download is sent to.
    req.add_unredirected_header("Authorization", "Bearer " + os.environ["GITHUB_TOKEN"])
    with urllib.request.urlopen(req, timeout=300) as r:
        data = r.read()
    return data if raw else json.loads(data)


def finished_run(commit):
    deadline = time.time() + 3600
    while True:
        runs = get(f"/repos/{REPO}/actions/workflows/release.yml/runs?head_sha={commit}&event=push&per_page=5")["workflow_runs"]
        run = next((r for r in runs if r["head_branch"].startswith("v")), None)
        if run and run["status"] == "completed":
            return run
        if time.time() > deadline:
            sys.exit(f"ci_files.py: no Release run for {commit} finished within an hour")
        time.sleep(30)


def main():
    commit, out = sys.argv[1], sys.argv[2]
    os.makedirs(out, exist_ok=True)
    run = finished_run(commit)
    if run["conclusion"] != "success":
        sys.exit(f"ci_files.py: the Release run for {run['head_branch']} ended {run['conclusion']}: {run['html_url']}")
    artifacts = {a["name"]: a for a in get(f"/repos/{REPO}/actions/runs/{run['id']}/artifacts?per_page=100")["artifacts"]}
    for name in FILES:
        a = artifacts.get(name)
        if not a or a["expired"]:
            sys.exit(f"ci_files.py: {run['html_url']} has no {name}")
        with zipfile.ZipFile(io.BytesIO(get(a["archive_download_url"], raw=True))) as z:
            data = z.read(name)
        digest = hashlib.sha256(data).hexdigest()
        try:
            attested = get(f"/repos/{REPO}/attestations/sha256:{digest}")["attestations"]
        except urllib.error.HTTPError as e:
            attested = [] if e.code == 404 else sys.exit(f"ci_files.py: attestations of {name}: {e}")
        if not attested:
            sys.exit(f"ci_files.py: {name} (sha256 {digest}) has no build attestation")
        path = os.path.join(out, name)
        with open(path, "wb") as f:
            f.write(data)
        os.chmod(path, 0o755)
        print(f"took {name} from {run['html_url']} (sha256 {digest}, attested)")


if __name__ == "__main__":
    main()
