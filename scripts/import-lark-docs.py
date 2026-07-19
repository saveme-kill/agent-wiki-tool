#!/usr/bin/env python3
import argparse
import html
import json
import os
import re
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Optional, Tuple
from urllib.parse import urlparse


@dataclass
class Doc:
    ordinal: int
    level: int
    title: str
    url: str
    token: str
    parent_token: Optional[str] = None
    source_page: str = ""


def run(args: List[str], *, input_text: Optional[str] = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        args,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
    )


def die(message: str) -> None:
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


def token_from_url(url: str) -> str:
    path = urlparse(url).path.rstrip("/")
    token = path.rsplit("/", 1)[-1]
    if not re.fullmatch(r"[A-Za-z0-9_-]+", token):
        die(f"cannot derive token from URL: {url}")
    return token


def parse_docs(manifest: Path) -> List[Doc]:
    rows: List[Doc] = []
    row_re = re.compile(
        r"^\|\s*(\d+)\s*\|\s*(\d+)\s*\|\s*(.*?)\s*\|\s*`([^`]+)`\s*\|\s*\[打开\]\(([^)]+)\)\s*\|"
    )
    for line in manifest.read_text(encoding="utf-8").splitlines():
        match = row_re.match(line)
        if not match:
            continue
        ordinal = int(match.group(1))
        level = int(match.group(2))
        title = html.unescape(match.group(3)).replace("&nbsp;", " ")
        title = re.sub(r"\s+", " ", title).strip()
        url = match.group(5).strip()
        rows.append(Doc(ordinal, level, title, url, token_from_url(url)))

    if not rows:
        die(f"no document rows found in {manifest}")

    stack: Dict[int, str] = {}
    for doc in rows:
        doc.parent_token = stack.get(doc.level - 1)
        stack[doc.level] = doc.token
        for key in list(stack):
            if key > doc.level:
                del stack[key]
    return rows


def markdown_escape_table(text: str) -> str:
    return text.replace("|", "\\|").replace("\n", " ")


def truncate_utf8(text: str, max_bytes: int = 180) -> str:
    out: List[str] = []
    total = 0
    for ch in text:
        size = len(ch.encode("utf-8"))
        if total + size > max_bytes:
            break
        out.append(ch)
        total += size
    return "".join(out).rstrip(" .-")


def title_filename_stem(title: str, fallback: str) -> str:
    stem = html.unescape(title).replace("&nbsp;", " ")
    stem = re.sub(r"\s+", " ", stem).strip()
    if stem.lower().endswith(".md"):
        stem = stem[:-3].rstrip()
    # Keep human-readable CJK/space characters, but remove path separators and
    # characters that are special in wikilinks or common filesystems.
    stem = re.sub(r'[\\/:*?"<>|#\[\]\x00-\x1f]', "-", stem)
    stem = re.sub(r"-{2,}", "-", stem).strip(" .-")
    stem = truncate_utf8(stem)
    return stem or fallback


def assign_source_pages(docs: List[Doc]) -> None:
    used: Dict[str, str] = {}
    for doc in docs:
        base = title_filename_stem(doc.title, doc.token)
        stem = base
        if stem in used and used[stem] != doc.token:
            stem = title_filename_stem(f"{base}-{doc.token[:8]}", doc.token)
            i = 2
            while stem in used and used[stem] != doc.token:
                stem = title_filename_stem(f"{base}-{doc.token[:8]}-{i}", doc.token)
                i += 1
        used[stem] = doc.token
        doc.source_page = stem


def source_ref(doc: Doc) -> str:
    return f"sources/{doc.source_page}"


def raw_ref(doc: Doc) -> str:
    return f"raw/feishu/{doc.source_page}.md"


def raw_dest(doc: Doc) -> str:
    return f"feishu/{doc.source_page}.md"


def source_body(doc: Doc, by_token: Dict[str, Doc], children: Dict[str, List[Doc]]) -> str:
    parent = by_token.get(doc.parent_token or "")
    child_docs = children.get(doc.token, [])
    lines = [
        f"# {doc.title}",
        "",
        "This source page tracks one imported Feishu wiki document.",
        "",
        "## Provenance",
        "",
        f"- Manifest row: {doc.ordinal}",
        f"- Feishu URL: {doc.url}",
        f"- Raw source: `{raw_ref(doc)}`",
        f"- Feishu token: `{doc.token}`",
        f"- Hierarchy level: {doc.level}",
    ]
    if parent is not None:
        lines.append(f"- Parent source: [[{source_ref(parent)}]]")
    if child_docs:
        lines.append("- Child sources:")
        for child in child_docs:
            lines.append(f"  - [[{source_ref(child)}]] - {child.title}")
    lines.extend(
        [
            "",
            "## Notes",
            "",
            "Imported from Feishu as Markdown. Review the raw source for full content before promoting facts into entity or concept pages.",
        ]
    )
    if parent is not None:
        lines.extend(
            [
                "",
                "## Context",
                "",
                f"This document is part of [[{source_ref(parent)}]].",
            ]
        )
    return "\n".join(lines) + "\n"


def manifest_body(docs: List[Doc]) -> str:
    lines = [
        "# 计量计费文档清单",
        "",
        "This source page is the import manifest for the Feishu wiki export list in `raw/lark-doc.md`.",
        "",
        "## Imported Documents",
        "",
        "| # | Level | Title | Source page |",
        "|---:|---:|---|---|",
    ]
    for doc in docs:
        lines.append(
            f"| {doc.ordinal} | {doc.level} | {markdown_escape_table(doc.title)} | [[{source_ref(doc)}]] |"
        )
    return "\n".join(lines) + "\n"


def fetch_markdown(doc: Doc) -> str:
    proc = run(
        [
            "lark-cli",
            "docs",
            "+fetch",
            "--api-version",
            "v2",
            "--as",
            "user",
            "--doc",
            doc.url,
            "--doc-format",
            "markdown",
            "--format",
            "json",
        ]
    )
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip() or "fetch failed")
    payload = json.loads(proc.stdout)
    if not payload.get("ok"):
        raise RuntimeError(json.dumps(payload.get("error", payload), ensure_ascii=False))
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


def write_page(
    wiki_bin: str,
    workspace: Path,
    page: str,
    title: str,
    source: str,
    body: str,
    *,
    origin: Optional[str] = None,
    tags: Optional[List[str]] = None,
) -> None:
    args = [
        wiki_bin,
        "-C",
        str(workspace),
        "write",
        page,
        "--type",
        "source",
        "--title",
        title,
        "--source",
        source,
        "--status",
        "stub",
        "--json",
    ]
    if origin:
        args.extend(["--origin", origin])
    for tag in tags or []:
        args.extend(["--tag", tag])
    proc = run(args, input_text=body)
    if proc.returncode != 0:
        raise RuntimeError(proc.stderr.strip() or proc.stdout.strip())


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--manifest", default="lark-doc.md")
    parser.add_argument("--workspace", default="my-wiki")
    parser.add_argument("--wiki-bin", default="target/debug/wiki")
    parser.add_argument("--overwrite-raw", action="store_true")
    parser.add_argument("--limit", type=int, default=0)
    args = parser.parse_args()

    manifest = Path(args.manifest)
    workspace = Path(args.workspace)
    wiki_bin = args.wiki_bin
    docs = parse_docs(manifest)
    if args.limit:
        docs = docs[: args.limit]
    assign_source_pages(docs)

    by_token = {doc.token: doc for doc in docs}
    children: Dict[str, List[Doc]] = {}
    for doc in docs:
        if doc.parent_token:
            children.setdefault(doc.parent_token, []).append(doc)

    print(f"parsed {len(docs)} docs", file=sys.stderr)

    manifest_raw = "lark-doc.md"
    if args.overwrite_raw or not (workspace / "raw" / manifest_raw).exists():
        add_raw(wiki_bin, workspace, manifest_raw, manifest.read_text(encoding="utf-8"), args.overwrite_raw)
    write_page(
        wiki_bin,
        workspace,
        "sources/lark-doc-manifest",
        "计量计费文档清单",
        f"raw/{manifest_raw}",
        manifest_body(docs),
        tags=["feishu", "manifest", "计量计费"],
    )

    failures: List[Tuple[Doc, str]] = []
    for index, doc in enumerate(docs, start=1):
        raw_dest_name = raw_dest(doc)
        raw_path = workspace / "raw" / raw_dest_name
        try:
            if args.overwrite_raw or not raw_path.exists():
                print(f"[{index}/{len(docs)}] fetch {doc.ordinal}: {doc.title}", file=sys.stderr)
                markdown = fetch_markdown(doc)
                stamped = (
                    f"<!-- origin: {doc.url} -->\n"
                    f"<!-- fetched: 2026-07-07 -->\n"
                    f"<!-- title: {doc.title} -->\n\n"
                    f"{markdown.rstrip()}\n"
                )
                add_raw(wiki_bin, workspace, raw_dest_name, stamped, args.overwrite_raw)
            else:
                print(f"[{index}/{len(docs)}] raw exists {doc.ordinal}: {doc.title}", file=sys.stderr)
            write_page(
                wiki_bin,
                workspace,
                source_ref(doc),
                doc.title,
                raw_ref(doc),
                source_body(doc, by_token, children),
                origin=doc.url,
                tags=["feishu", "计量计费"],
            )
        except Exception as exc:
            failures.append((doc, str(exc)))
            print(f"FAILED {doc.ordinal} {doc.token}: {exc}", file=sys.stderr)

    if failures:
        print("\nfailures:", file=sys.stderr)
        for doc, message in failures:
            print(f"- {doc.ordinal} {doc.title} {doc.url}: {message}", file=sys.stderr)
        sys.exit(2)


if __name__ == "__main__":
    main()
