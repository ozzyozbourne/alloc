"""Builds each folder's guide.html (and the root index.html) from tools/guides/*.html.

Run from the repository root:

    uv run --with pygments python tools/gen_guides.py

Templates are plain HTML with directives that pull in real code, so the guides
always match the sources:

    {{c-item  original-c/file.c  start-text}}  a top-level C function/struct
    {{rs-item src/lib.rs         start-text}}  a Rust item (fn, struct, impl)
    {{c-lines original-c/file.h 98-99}}        a fixed line range (for macros)
    {{c-file  original-c/file.c}}              a whole file, collapsed
    {{rs-file src/lib.rs}}                     a whole file, collapsed
    {{output  crate example}}                  stdout of `cargo run --example`

`start-text` is a literal substring of the item's first line. A directive that
matches nothing stops the build.
"""

import html
import pathlib
import re
import subprocess
import sys

from pygments import highlight
from pygments.formatters import HtmlFormatter
from pygments.lexers import CLexer, RustLexer, TextLexer

ROOT = pathlib.Path(__file__).resolve().parent.parent
TEMPLATES = ROOT / "tools" / "guides"
FORMATTER = HtmlFormatter(nowrap=True, noclasses=True, style="github-dark")

GUIDES = [
    ("01-kohi-linear-allocator", "Kohi · Linear allocator"),
    ("02-kohi-pool-allocator", "Kohi · Pool allocator"),
    ("03-kohi-freelist", "Kohi · Freelist"),
    ("04-kohi-dynamic-allocator", "Kohi · Dynamic allocator"),
    ("05-rad-arena", "RAD · Arena"),
    ("06-rad-scratch-arenas", "RAD · Scratch arenas"),
    ("07-rad-thread-pool-arenas", "RAD · Thread-pool arenas"),
]

CSS = """
:root { --fg:#1f2328; --muted:#59636e; --line:#d1d9e0; --bg:#ffffff; --soft:#f6f8fa;
        --accent:#0969da; --c:#b35900; --rs:#a0410d; --good:#1a7f37; --bad:#cf222e; }
* { box-sizing: border-box; }
body { margin:0; font:16px/1.6 -apple-system, "Segoe UI", Helvetica, Arial, sans-serif; color:var(--fg); background:var(--bg); }
.layout { display:grid; grid-template-columns: 250px minmax(0,1fr); min-height:100vh; }
nav { border-right:1px solid var(--line); background:var(--soft); padding:20px 16px; position:sticky; top:0; height:100vh; overflow:auto; font-size:14px; }
nav a { display:block; color:var(--fg); text-decoration:none; padding:4px 8px; border-radius:6px; }
nav a:hover { background:#eaeef2; }
nav a.here { background:var(--accent); color:#fff; }
nav h2 { font-size:12px; text-transform:uppercase; letter-spacing:.06em; color:var(--muted); margin:18px 8px 6px; }
nav .toc a { color:var(--muted); padding-left:16px; }
main { padding:32px 48px 80px; max-width:1180px; }
h1 { font-size:32px; margin:0 0 4px; }
h2 { font-size:24px; margin:48px 0 12px; padding-bottom:6px; border-bottom:1px solid var(--line); }
h3 { font-size:18px; margin:28px 0 8px; }
.lede { font-size:18px; color:var(--muted); margin:0 0 24px; }
code { font:13.5px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; background:var(--soft); padding:1px 5px; border-radius:4px; }
pre { margin:0; padding:14px 16px; overflow:auto; background:#0d1117; color:#e6edf3; border-radius:0 0 8px 8px;
      font:13px/1.5 ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; }
pre code { background:none; padding:0; }
figure.code { margin:14px 0; border:1px solid #30363d; border-radius:8px; }
figure.code figcaption { font:12px ui-monospace, monospace; padding:6px 12px; background:#161b22; color:#9198a1; border-radius:8px 8px 0 0; display:flex; justify-content:space-between; gap:12px; }
figure.code figcaption .lang { font-weight:700; }
figure.code.c figcaption .lang { color:#f0883e; }
figure.code.rs figcaption .lang { color:#ff7b72; }
figure.code.out figcaption .lang { color:#7ee787; }
figure.code figcaption a { color:#9198a1; }
details.file { margin:12px 0; }
details.file > summary { cursor:pointer; color:var(--accent); font-weight:600; }
.pair { display:grid; grid-template-columns: 1fr 1fr; gap:16px; align-items:start; }
.pair > div > h4 { margin:8px 0 0; font-size:13px; text-transform:uppercase; letter-spacing:.05em; }
.pair > div > h4.c { color:var(--c); } .pair > div > h4.rs { color:var(--rs); }
.note { border-left:4px solid var(--accent); background:#ddf4ff; padding:10px 14px; border-radius:0 6px 6px 0; margin:14px 0; }
.warn { border-left:4px solid #bf8700; background:#fff8c5; padding:10px 14px; border-radius:0 6px 6px 0; margin:14px 0; }
.bug  { border-left:4px solid var(--bad); background:#ffebe9; padding:10px 14px; border-radius:0 6px 6px 0; margin:14px 0; }
table { border-collapse:collapse; width:100%; margin:14px 0; font-size:14.5px; }
th, td { border:1px solid var(--line); padding:7px 10px; text-align:left; vertical-align:top; }
th { background:var(--soft); }
td.yes { color:var(--good); font-weight:600; } td.no { color:var(--bad); font-weight:600; }
.diagram { background:var(--soft); border:1px solid var(--line); border-radius:8px; padding:14px; overflow:auto; }
.diagram svg { display:block; margin:auto; max-width:100%; height:auto; }
ol.steps > li { margin:10px 0; }
@media (max-width: 1000px) { .layout { grid-template-columns: 1fr; } nav { position:static; height:auto; } .pair { grid-template-columns: 1fr; } main { padding:20px; } }
"""


def die(msg):
    sys.exit(f"gen_guides: {msg}")


def read(path):
    return path.read_text().replace("\r", "").split("\n")


def figure(kind, lang_label, lines, start, path, folder, lexer):
    code = "\n".join(lines).rstrip("\n")
    end = start + len(lines) - 1
    rel = path.relative_to(ROOT / folder) if folder else path.relative_to(ROOT)
    where = f"lines {start}–{end}" if start else ""
    return (
        f'<figure class="code {kind}"><figcaption><span><span class="lang">{lang_label}</span> '
        f'<a href="{rel.as_posix()}">{html.escape(rel.as_posix())}</a></span><span>{where}</span></figcaption>'
        f"<pre><code>{highlight(code, lexer, FORMATTER)}</code></pre></figure>"
    )


def c_item(path, needle):
    lines = read(path)
    for i, line in enumerate(lines):
        if needle in line and not line.rstrip().endswith(";"):
            break
    else:
        die(f"{path}: no line containing {needle!r}")
    start = i
    # Pull in return-type lines and comments directly above (RAD puts the
    # return type on its own line).
    while start > 0 and lines[start - 1].strip() and not lines[start - 1].startswith("}") \
            and not lines[start - 1].startswith("#"):
        start -= 1
    end = i
    while end < len(lines) and not lines[end].startswith("}"):
        end += 1
    return lines[start:end + 1], start + 1


def rs_item(path, needle):
    lines = read(path)
    for i, line in enumerate(lines):
        if needle in line and not line.lstrip().startswith("//"):
            break
    else:
        die(f"{path}: no line containing {needle!r}")
    indent = len(lines[i]) - len(lines[i].lstrip())
    start = i
    while start > 0 and lines[start - 1].strip().startswith(("///", "#[", "// SAFETY", "//")) \
            and len(lines[start - 1]) - len(lines[start - 1].lstrip()) == indent:
        start -= 1
    end = i
    if not lines[i].rstrip().endswith(";"):
        close = " " * indent + "}"
        while end < len(lines) and lines[end].rstrip() != close and not lines[end].rstrip().startswith(close + ")"):
            end += 1
        if end == len(lines):
            die(f"{path}: item {needle!r} never closes")
    return lines[start:end + 1], start + 1


def run_example(crate, example):
    res = subprocess.run(["cargo", "run", "-q", "-p", crate, "--example", example],
                         cwd=ROOT, capture_output=True, text=True)
    if res.returncode != 0:
        die(f"example {crate}/{example} failed:\n{res.stderr}")
    # Heap addresses change from run to run; show them as placeholders.
    return re.sub(r"0x[0-9a-f]{11,}", "0x…", res.stdout).rstrip("\n").split("\n")


def expand(text, folder):
    base = ROOT / folder if folder else ROOT

    def sub(m):
        kind, args = m.group(1), m.group(2).split(None, 1)
        if kind in ("c-item", "rs-item"):
            path = base / args[0]
            if kind == "c-item":
                lines, start = c_item(path, args[1])
                return figure("c", "C", lines, start, path, folder, CLexer())
            lines, start = rs_item(path, args[1])
            return figure("rs", "Rust", lines, start, path, folder, RustLexer())
        if kind == "c-lines":
            path = base / args[0]
            first, last = (int(n) for n in args[1].split("-"))
            return figure("c", "C", read(path)[first - 1:last], first, path, folder, CLexer())
        if kind in ("c-file", "rs-file"):
            path = base / args[0]
            lines = read(path)
            c = kind == "c-file"
            fig = figure("c" if c else "rs", "C" if c else "Rust", lines, 1, path, folder,
                         CLexer() if c else RustLexer())
            return f'<details class="file"><summary>Show the whole file: {html.escape(args[0])} ({len(lines)} lines)</summary>{fig}</details>'
        if kind == "output":
            crate, example = args[0], args[1]
            out = run_example(crate, example)
            return (f'<figure class="code out"><figcaption><span><span class="lang">Output</span> '
                    f'cargo run -p {crate} --example {example}</span></figcaption>'
                    f'<pre><code>{highlight(chr(10).join(out), TextLexer(), FORMATTER)}</code></pre></figure>')
        die(f"unknown directive {kind}")

    return re.sub(r"\{\{(c-item|rs-item|c-lines|c-file|rs-file|output)\s+([^}]+?)\}\}", sub, text)


def page(title, body, here, prefix):
    links = [f'<a href="{prefix}index.html" class="{"here" if here is None else ""}">Overview</a>']
    for folder, label in GUIDES:
        cls = "here" if folder == here else ""
        links.append(f'<a href="{prefix}{folder}/guide.html" class="{cls}">{html.escape(label)}</a>')
    toc = "".join(
        f'<a href="#{m.group(1)}">{m.group(2)}</a>'
        for m in re.finditer(r'<h2 id="([^"]+)">(.*?)</h2>', body)
    )
    return f"""<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{html.escape(title)} — C allocators in Rust</title>
<style>{CSS}</style></head>
<body><div class="layout">
<nav><h2>Guides</h2>{''.join(links)}<h2>On this page</h2><div class="toc">{toc}</div></nav>
<main>{body}</main>
</div></body></html>
"""


def main():
    for folder, label in GUIDES:
        template = TEMPLATES / f"{folder}.html"
        body = expand(template.read_text(), folder)
        (ROOT / folder / "guide.html").write_text(page(label, body, folder, "../"))
        print(f"wrote {folder}/guide.html")
    body = expand((TEMPLATES / "index.html").read_text(), None)
    (ROOT / "index.html").write_text(page("Overview", body, None, ""))
    print("wrote index.html")


if __name__ == "__main__":
    main()
