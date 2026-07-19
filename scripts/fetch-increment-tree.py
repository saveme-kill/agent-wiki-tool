#!/usr/bin/env python3
"""
fetch-increment-tree.py — fetch the Feishu "增量框架" wiki tree into a wiki
workspace's raw/ layer, preserving the nested-bullet hierarchy.

Unlike scripts/import-lark-docs.py (which parses a markdown *table* manifest),
this reads the nested-bullet format of wiki_tree_urls.md where indentation
encodes the parent/child hierarchy:

    - [Title](url)（docx）
      - [Child](url)（docx）
        - [Grandchild](url)（docx）

It fetches each doc as Markdown via lark-cli (user identity), stamps origin/
fetched/title as HTML comments, and registers it under raw/feishu/ via
`wiki add-raw`. It writes a JSON manifest (default: increment-manifest.json)
capturing ordinal/level/title/url/token/doctype/parent_token/slug so a later
step can build source pages and the overview from the same hierarchy.

Fetching is network-only here; the Rust core stays network-free.
"""
import argparse
import html
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Dict, List, Optional
from urllib.parse import urlparse


@dataclass
class Doc:
    ordinal: int
    level: int
    title: str
    url: str
    token: str
    doctype: str
    parent_token: Optional[str] = None
    slug: str = ""
    raw_file: str = ""
    fetched: bool = False
    error: str = ""


def run(args: List[str], *, input_text: Optional[str] = None) -> subprocess.CompletedProcess:
    return subprocess.run(args, input=input_text, text=True,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE)


def die(msg: str) -> None:
    print(f"error: {msg}", file=sys.stderr)
    sys.exit(1)


def token_from_url(url: str) -> str:
    path = urlparse(url).path.rstrip("/")
    token = path.rsplit("/", 1)[-1]
    if not re.fullmatch(r"[A-Za-z0-9_-]+", token):
        die(f"cannot derive token from URL: {url}")
    return token


# Match:  <indent>- [Title](URL)（doctype）   — doctype paren is optional.
BULLET_RE = re.compile(
    r"^(?P<indent>[ \t]*)-\s+\[(?P<title>.*?)\]\((?P<url>[^)]+)\)\s*(?:[（(](?P<doctype>[^）)]*)[）)])?\s*$"
)


def parse_tree(manifest: Path) -> List[Doc]:
    raw_rows: List[tuple] = []  # (indent_cols, title, url, doctype)
    for line in manifest.read_text(encoding="utf-8").splitlines():
        m = BULLET_RE.match(line)
        if not m:
            continue
        indent = m.group("indent").replace("\t", "    ")
        title = re.sub(r"\s+", " ", html.unescape(m.group("title"))).strip()
        url = m.group("url").strip()
        doctype = (m.group("doctype") or "").strip()
        raw_rows.append((len(indent), title, url, doctype))

    if not raw_rows:
        die(f"no bullet rows found in {manifest}")

    # Map distinct indent widths → 1-based levels (robust to 2- or 4-space steps).
    widths = sorted({w for w, *_ in raw_rows})
    level_of = {w: i + 1 for i, w in enumerate(widths)}

    docs: List[Doc] = []
    for ordinal, (w, title, url, doctype) in enumerate(raw_rows, start=1):
        docs.append(Doc(ordinal=ordinal, level=level_of[w], title=title,
                        url=url, token=token_from_url(url), doctype=doctype))

    # Resolve parent via an indent stack.
    stack: Dict[int, str] = {}
    for doc in docs:
        doc.parent_token = stack.get(doc.level - 1)
        stack[doc.level] = doc.token
        for k in [k for k in stack if k > doc.level]:
            del stack[k]
    return docs


def truncate_utf8(text: str, max_bytes: int = 180) -> str:
    out, total = [], 0
    for ch in text:
        size = len(ch.encode("utf-8"))
        if total + size > max_bytes:
            break
        out.append(ch)
        total += size
    return "".join(out).rstrip(" .-")


def title_stem(title: str, fallback: str) -> str:
    stem = re.sub(r"\s+", " ", html.unescape(title)).strip()
    if stem.lower().endswith(".md"):
        stem = stem[:-3].rstrip()
    stem = re.sub(r'[\\/:*?"<>|#\[\]\x00-\x1f]', "-", stem)
    stem = re.sub(r"-{2,}", "-", stem).strip(" .-")
    stem = truncate_utf8(stem)
    return stem or fallback


def assign_slugs(docs: List[Doc]) -> None:
    used: Dict[str, str] = {}
    for doc in docs:
        base = title_stem(doc.title, doc.token)
        stem = base
        i = 2
        while stem in used and used[stem] != doc.token:
            stem = title_stem(f"{base}-{doc.token[:8]}", doc.token)
            if stem in used and used[stem] != doc.token:
                stem = title_stem(f"{base}-{doc.token[:8]}-{i}", doc.token)
                i += 1
        used[stem] = doc.token
        doc.slug = stem
        doc.raw_file = f"feishu/{stem}.md"


def fetch_markdown(doc: Doc, identity: str) -> str:
    proc = run(["lark-cli", "docs", "+fetch", "--api-version", "v2",
                "--as", identity, "--doc", doc.url,
                "--doc-format", "markdown", "--format", "json"])
    if proc.returncode != 0 and not proc.stdout.strip():
        raise RuntimeError(proc.stderr.strip() or "fetch failed (no output)")
    payload = json.loads(proc.stdout)
    if not payload.get("ok"):
        err = payload.get("error", {})
        raise RuntimeError(err.get("message") or json.dumps(err, ensure_ascii=False))
    content = payload.get("data", {}).get("document", {}).get("content", "")
    if not content.strip():
        raise RuntimeError("fetched document is empty")
    return content


def add_raw(wiki_bin: str, workspace: Path, dest: str, content: str, overwrite: bool) -> None:
    with tempfile.NamedTemporaryFile("w", encoding="utf-8", delete=False) as tmp:
        tmp.write(content)
        tmp_path = tmp.name
    try:
        args = [wiki_bin, "-C", str(workspace), "add-raw", tmp_path, "--name", dest, "--json"]
        if overwrite:
            args.append("--overwrite")
        proc = run(args)
        if proc.returncode != 0:
            raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())
    finally:
        os.unlink(tmp_path)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", default="wiki_tree_urls.md")
    ap.add_argument("--workspace", default="increment-wiki")
    ap.add_argument("--wiki-bin", default="target/release/wiki")
    ap.add_argument("--identity", default="user")
    ap.add_argument("--out", default="increment-manifest.json")
    ap.add_argument("--overwrite-raw", action="store_true")
    ap.add_argument("--limit", type=int, default=0)
    args = ap.parse_args()

    workspace = Path(args.workspace)
    docs = parse_tree(Path(args.manifest))
    if args.limit:
        docs = docs[: args.limit]
    assign_slugs(docs)
    print(f"parsed {len(docs)} docs (levels "
          f"{sorted({d.level for d in docs})})", file=sys.stderr)

    fetched_date = "2026-07-09"
    failures: List[Doc] = []
    for i, doc in enumerate(docs, start=1):
        raw_path = workspace / "raw" / doc.raw_file
        try:
            if args.overwrite_raw or not raw_path.exists():
                print(f"[{i}/{len(docs)}] fetch #{doc.ordinal} L{doc.level} "
                      f"({doc.doctype}): {doc.title}", file=sys.stderr)
                md = fetch_markdown(doc, args.identity)
                stamped = (f"<!-- origin: {doc.url} -->\n"
                           f"<!-- fetched: {fetched_date} -->\n"
                           f"<!-- title: {doc.title} -->\n"
                           f"<!-- doctype: {doc.doctype} -->\n\n"
                           f"{md.rstrip()}\n")
                add_raw(args.wiki_bin, workspace, doc.raw_file, stamped, args.overwrite_raw)
            else:
                print(f"[{i}/{len(docs)}] exists #{doc.ordinal}: {doc.title}", file=sys.stderr)
            doc.fetched = True
        except Exception as exc:  # noqa: BLE001 — record and continue
            doc.error = str(exc)
            failures.append(doc)
            print(f"  FAILED #{doc.ordinal} {doc.token}: {exc}", file=sys.stderr)

    Path(args.out).write_text(
        json.dumps([asdict(d) for d in docs], ensure_ascii=False, indent=2),
        encoding="utf-8")
    ok = sum(1 for d in docs if d.fetched)
    print(f"\nmanifest → {args.out} | fetched {ok}/{len(docs)}", file=sys.stderr)
    if failures:
        print("failures:", file=sys.stderr)
        for d in failures:
            print(f"- #{d.ordinal} [{d.doctype}] {d.title} {d.url}: {d.error}", file=sys.stderr)


if __name__ == "__main__":
    main()
