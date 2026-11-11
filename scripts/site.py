"""Puts the site and the downloads into OUT for sshremote.dev:

    python scripts/site.py OUT [--download NAME=FILE ...]

site/ is copied as it is; each download goes to OUT/download with its size and SHA-256 listed in
OUT/download/index.json, beside the release's version from Cargo.toml.
"""
import hashlib
import json
import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def main():
    out = Path(sys.argv[1])
    for f in (ROOT / "site").iterdir():
        if f.is_file():
            shutil.copy(f, out / f.name)
        elif f.is_dir():
            shutil.copytree(f, out / f.name, dirs_exist_ok=True)
    args = sys.argv[2:]
    downloads = out / "download"
    downloads.mkdir(exist_ok=True)
    files = []
    for i, a in enumerate(args):
        if a == "--download":
            name, file = args[i + 1].split("=", 1)
            shutil.copy(file, downloads / name)
            data = (downloads / name).read_bytes()
            files.append({"name": name, "size": len(data), "sha256": hashlib.sha256(data).hexdigest()})
    cargo = (ROOT / "Cargo.toml").read_text(encoding="utf-8").splitlines()
    version = next((l.split('"')[1] for l in cargo if l.startswith("version")), "")
    (downloads / "index.json").write_text(json.dumps({"version": version, "files": files}, indent=1))


if __name__ == "__main__":
    main()
