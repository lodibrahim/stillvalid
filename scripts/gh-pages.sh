#!/usr/bin/env bash
# Keeps the Action's copy of the gh-pages branch in a separate repo at $PAGES_DIR,
# so the workflow's own checkout is never touched.
#
#   gh-pages.sh fetch      check out stillvalid/ from gh-pages (depth 1, blobless, sparse)
#                          into $PAGES_DIR, or start an empty orphan branch if the remote has none
#   gh-pages.sh publish    copy $SITE_DIR into stillvalid/, commit only if something changed, push
#
# Env: PAGES_DIR (required); SITE_DIR (publish); PAGES_REMOTE (default $GITHUB_SERVER_URL/$GITHUB_REPOSITORY.git);
# GITHUB_TOKEN (optional), passed to git only through GIT_CONFIG_* env vars.
set -euo pipefail

: "${PAGES_DIR:?PAGES_DIR is required}"
remote="${PAGES_REMOTE:-${GITHUB_SERVER_URL:-https://github.com}/${GITHUB_REPOSITORY:?}.git}"

if [ -n "${GITHUB_TOKEN:-}" ]; then
  auth="$(printf 'x-access-token:%s' "$GITHUB_TOKEN" | base64 | tr -d '\n')"
  if [ "${GITHUB_ACTIONS:-}" = "true" ]; then
    echo "::add-mask::$auth"
  fi
  export GIT_CONFIG_COUNT=1
  export GIT_CONFIG_KEY_0="http.${GITHUB_SERVER_URL:-https://github.com}/.extraheader"
  export GIT_CONFIG_VALUE_0="AUTHORIZATION: basic $auth"
fi

case "${1:-}" in
  fetch)
    rm -rf "$PAGES_DIR"
    git init -q "$PAGES_DIR"
    cd "$PAGES_DIR"
    git remote add origin "$remote"
    git symbolic-ref HEAD refs/heads/gh-pages
    status=0
    git ls-remote --exit-code origin refs/heads/gh-pages >/dev/null || status=$?
    if [ "$status" -eq 0 ]; then
      echo /stillvalid/ | git sparse-checkout set --no-cone --stdin
      git fetch -q --depth 1 --filter=blob:none --no-tags origin +refs/heads/gh-pages:refs/remotes/origin/gh-pages
      git checkout -q -B gh-pages origin/gh-pages
    elif [ "$status" -eq 2 ]; then
      echo "stillvalid: no gh-pages branch yet; first run"
    else
      exit "$status"
    fi
    ;;
  publish)
    : "${SITE_DIR:?SITE_DIR is required}"
    mkdir -p "$PAGES_DIR/stillvalid"
    cp -R "$SITE_DIR/." "$PAGES_DIR/stillvalid/"
    cd "$PAGES_DIR"
    git add stillvalid
    if git diff --cached --quiet; then
      echo "stillvalid: gh-pages already up to date"
      exit 0
    fi
    sha="${GITHUB_SHA:-}"
    git -c user.name="github-actions[bot]" \
      -c user.email="41898282+github-actions[bot]@users.noreply.github.com" \
      commit -q -m "stillvalid report for ${sha:0:7}"
    git push -q origin HEAD:refs/heads/gh-pages
    echo "stillvalid: published to gh-pages"
    ;;
  *)
    echo "usage: gh-pages.sh fetch | publish" >&2
    exit 2
    ;;
esac
