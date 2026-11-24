#!/bin/sh
# Publishes the release scripts/release.sh put together in OUT:
#   GITHUB_TOKEN=... SIGN_HOST=... [FIRST_SIGN_HOST=...] scripts/publish.sh OUT [SSH-HOST] [WEB-ROOT]
# On GitHub first, as the site's lists and the installers point there; then the site with its
# signed lists (scripts/deploy.sh, which takes SSH-HOST and WEB-ROOT); then the package managers
# (scripts/packages.py). See docs/releasing.md.
set -eu
OUT=$(cd "$1" && pwd)
shift
HERE=$(cd "$(dirname "$0")" && pwd)
python3 "$HERE/gh_release.py" publish "$OUT"
RELEASE_DIR="$OUT" sh "$HERE/deploy.sh" "$@"
python3 "$HERE/packages.py" "$OUT"
