#!/usr/bin/env bash
#
# feishu-ingest.sh — bridge a Feishu (Lark) online document into a wiki
# workspace's immutable raw/ layer.
#
# agent-wiki's `wiki-core` is deliberately network-free: `add-raw` only reads
# local UTF-8 files. Feishu docs are authenticated online content, so this thin
# shell bridge does the fetching *outside* the core: it uses `lark-cli` to
# export the doc as Markdown, stamps the file with the origin URL as HTML
# comments, and hands it to `wiki add-raw` under raw/feishu/. The Rust code
# stays unchanged and unaware of the network.
#
# Usage:
#   scripts/feishu-ingest.sh <FEISHU_URL> [--workspace DIR] [--name SLUG]
#
# Options:
#   --workspace DIR   Wiki workspace root (default: current directory).
#   --name SLUG       Destination slug under raw/feishu/ (default: derived from
#                     the Feishu document title, falling back to the URL token).
#                     The generated wiki source-page hint also uses the title.
#                     ".md" is appended automatically.
#
# Env:
#   WIKI_BIN          Path to the `wiki` binary. If unset, the script looks for
#                     target/release/wiki then target/debug/wiki under the repo,
#                     then falls back to `wiki` on PATH.
#
# Requires: lark-cli (run `lark-cli auth login` once), jq, and a built `wiki`.
set -euo pipefail

die() { printf 'error: %s\n' "$*" >&2; exit 1; }

# --- locate this script / repo so we can find the built `wiki` binary --------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

# --- parse arguments ---------------------------------------------------------
URL=""
WORKSPACE="."
NAME=""
while [ $# -gt 0 ]; do
  case "$1" in
    --workspace) WORKSPACE="${2:-}"; shift 2 ;;
    --workspace=*) WORKSPACE="${1#*=}"; shift ;;
    --name) NAME="${2:-}"; shift 2 ;;
    --name=*) NAME="${1#*=}"; shift ;;
    -h|--help)
      sed -n '2,29p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
      exit 0 ;;
    --*) die "unknown option: $1" ;;
    *)
      [ -z "$URL" ] || die "unexpected extra argument: $1"
      URL="$1"; shift ;;
  esac
done
[ -n "$URL" ] || die "missing <FEISHU_URL>; see --help"

# --- dependency checks -------------------------------------------------------
command -v lark-cli >/dev/null 2>&1 || die "lark-cli not found on PATH; install lark-cli and run 'lark-cli auth login'"
command -v jq >/dev/null 2>&1 || die "jq not found on PATH"

# Resolve the wiki binary.
WIKI="${WIKI_BIN:-}"
if [ -z "$WIKI" ]; then
  if [ -x "$REPO_ROOT/target/release/wiki" ]; then
    WIKI="$REPO_ROOT/target/release/wiki"
  elif [ -x "$REPO_ROOT/target/debug/wiki" ]; then
    WIKI="$REPO_ROOT/target/debug/wiki"
  elif command -v wiki >/dev/null 2>&1; then
    WIKI="wiki"
  else
    die "no wiki binary found; run 'cargo build --release' or set WIKI_BIN"
  fi
fi

# --- derive a fallback slug from URL token ------------------------------------
# The final raw filename is derived from the Feishu document title after fetch;
# this token slug is only used as fallback, or when --name is explicitly passed.
if [ -n "$NAME" ]; then
  slug="$NAME"
else
  # Strip query string and fragment, then take the last path segment.
  token="${URL%%\?*}"; token="${token%%#*}"; token="${token##*/}"
  slug="$token"
fi
# Sanitize: keep [A-Za-z0-9_-], collapse runs, trim, drop any trailing ".md".
slug="${slug%.md}"
slug="$(printf '%s' "$slug" | tr -c 'A-Za-z0-9_-' '-' | sed -e 's/-\{2,\}/-/g' -e 's/^-//' -e 's/-$//')"
[ -n "$slug" ] || die "could not derive a slug from URL or --name; pass --name SLUG"

# --- fetch the document as Markdown via lark-cli -----------------------------
echo "Fetching Feishu document as Markdown…" >&2
fetch_json=""
if ! fetch_json="$(lark-cli docs +fetch --api-version v2 --as user --doc "$URL" --doc-format markdown --format json 2>/dev/null)"; then
  die "lark-cli fetch failed. If you are not authenticated, run: lark-cli auth login"
fi
ok="$(printf '%s' "$fetch_json" | jq -r '.ok // false' 2>/dev/null || echo false)"
if [ "$ok" != "true" ]; then
  msg="$(printf '%s' "$fetch_json" | jq -r '.error.message // .error // .message // "unknown error"' 2>/dev/null || echo 'unknown error')"
  die "lark-cli fetch returned an error: $msg. If this is an auth/permission issue, run: lark-cli auth login"
fi
content="$(printf '%s' "$fetch_json" | jq -r '.data.document.content // ""')"
[ -n "$content" ] || die "fetched document is empty (nothing to ingest)"
# Best-effort human title: first ATX heading in the Markdown (for the hint only).
title="$(printf '%s\n' "$content" | grep -m1 '^# ' | sed 's/^# *//' || true)"
page_slug="${title:-$slug}"
page_slug="${page_slug%.md}"
page_slug="$(printf '%s' "$page_slug" | sed -e 's@[][/:*?"<>|#\]@-@g' -e 's/[[:space:]][[:space:]]*/ /g' -e 's/-\{2,\}/-/g' -e 's/^[ .-]*//' -e 's/[ .-]*$//')"
[ -n "$page_slug" ] || page_slug="$slug"
# Default raw filename to the human title. An explicit --name still wins.
if [ -z "$NAME" ]; then
  slug="$page_slug"
fi

# --- write a temp file stamped with origin metadata --------------------------
# HTML comments carry the provenance without polluting rendered prose, and stay
# greppable inside raw/. The file is written to a temp path first because
# `wiki add-raw` copies the file into raw/ and refuses to overwrite an existing
# raw source (raw is immutable).
fetched="$(date +%Y-%m-%d)"
tmp="$(mktemp "${TMPDIR:-/tmp}/feishu-ingest.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
{
  printf '<!-- origin: %s -->\n' "$URL"
  printf '<!-- fetched: %s -->\n\n' "$fetched"
  printf '%s\n' "$content"
} > "$tmp"

# --- register under raw/feishu/ via the wiki CLI -----------------------------
dest="feishu/$slug.md"
echo "Registering raw source at raw/$dest …" >&2
"$WIKI" -C "$WORKSPACE" add-raw "$tmp" --name "$dest" --json

# --- next-step hint for the agent --------------------------------------------
cat >&2 <<EOF

Done. Raw source stored at: raw/$dest
  origin:  $URL
  fetched: $fetched
${title:+  title:   $title}

Next steps (the agent writes the prose):
  1. Review the source in chunks:
       $WIKI -C "$WORKSPACE" read-raw "$dest"
  2. Write a source summary page, recording provenance as first-class fields:
       $WIKI -C "$WORKSPACE" write "sources/$page_slug" --type source \\
         --title "${title:-<title>}" \\
         --origin "$URL" \\
         --source "raw/$dest" \\
         --body '<summary with [[wikilinks]] to entities/concepts>'
  3. Update related entities/ and concepts/ pages, then: index → log → lint.
EOF
