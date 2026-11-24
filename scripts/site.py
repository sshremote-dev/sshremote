"""Puts the site, and a release's downloads, into OUT for sshremote.dev:

    python scripts/site.py OUT [--files DIR]

site/ is copied, with the download section's list of files written from release/platforms.json.
With --files, DIR holds every file of release/platforms.json (scripts/release.sh puts them there):
OUT/download/latest.json lists each with its system, processor, size, SHA-256 and place in the
GitHub release of the version in Cargo.toml (what the program updates from from 0.25.36, and the
installers read). OUT/download/index.json and the files beside it are the first list, for the
versions before, which read only these three names from it.
"""
import hashlib
import html
import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
REPO = "https://github.com/sshremote-dev/sshremote"
FIRST_LIST = ["sshremote-windows-x86_64.exe", "sshremote-linux-x86_64", "sshremote-macos.zip"]
ICONS = {"windows": "windows", "linux": "linux", "macos": "apple"}
MARK = "<!-- files from release/platforms.json -->"


def platforms():
    return json.loads((ROOT / "release" / "platforms.json").read_text(encoding="utf-8"))["platforms"]


def version():
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines()
    return next(line.split('"')[1] for line in cargo if line.startswith("version"))


def file_rows():
    row = (
        '    <a class="file" href="{href}" data-file="{file}"><span data-i="{icon}"></span><span><b>SSH Remote for {label}</b>'
        '<span class="meta">{detail}</span></span><span class="size"></span><span class="go">Download <span data-i="download"></span></span></a>'
    )
    return "\n".join(
        row.format(
            href=f"{REPO}/releases/latest/download/{p['file']}",
            file=p["file"],
            icon=ICONS[p["os"]],
            label=html.escape(p["label"]),
            detail=html.escape(p["detail"]),
        )
        for p in platforms()
    )


def main():
    out = Path(sys.argv[1])
    for f in (ROOT / "site").iterdir():
        if f.is_file():
            shutil.copy(f, out / f.name)
        elif f.is_dir():
            shutil.copytree(f, out / f.name, dirs_exist_ok=True)
    page = out / "index.html"
    text = page.read_text(encoding="utf-8")
    if text.count(MARK) != 1:
        sys.exit(f"site.py: site/index.html needs the line {MARK} once")
    page.write_text(text.replace(MARK, file_rows()), encoding="utf-8")
    args = sys.argv[2:]
    if not args:
        return
    if args[0] != "--files" or len(args) != 2:
        sys.exit(__doc__)
    have = Path(args[1])
    missing = [p["file"] for p in platforms() if not (have / p["file"]).is_file()]
    if missing:
        sys.exit(f"site.py: {have} lacks {', '.join(missing)}")
    ver = version()
    downloads = out / "download"
    downloads.mkdir(exist_ok=True)
    latest, first = [], []
    for p in platforms():
        data = (have / p["file"]).read_bytes()
        entry = {"name": p["file"], "size": len(data), "sha256": hashlib.sha256(data).hexdigest()}
        latest.append({**entry, "os": p["os"], "arch": p["arch"], "url": f"{REPO}/releases/download/v{ver}/{p['file']}"})
        if p["file"] in FIRST_LIST:
            shutil.copy(have / p["file"], downloads / p["file"])
            first.append(entry)
    (downloads / "latest.json").write_text(json.dumps({"version": ver, "files": latest}, indent=1))
    (downloads / "index.json").write_text(json.dumps({"version": ver, "files": first}, indent=1))


if __name__ == "__main__":
    main()
