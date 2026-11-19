#!/usr/bin/env python3
"""Check a built copy of the manual.

Fails on broken internal links or anchors, duplicate element IDs, pages
without a title, empty navigation links and unrendered Markdown or Liquid.

Usage: check-site.py SITE_DIRECTORY
"""

import html.parser
import os
import re
import sys
import urllib.parse

ARTIFACTS = [r"\{:\s*[.#]", r"\{%", r"\]\(", r"\|---"]
# The editing guide shows Liquid syntax on purpose.
ARTIFACT_EXEMPT = {"contributing-docs.html"}


class Page(html.parser.HTMLParser):
    """The IDs, duplicate IDs and link targets of one HTML page."""

    def __init__(self) -> None:
        super().__init__()
        self.ids: set[str] = set()
        self.duplicates: list[str] = []
        self.links: list[str] = []

    def handle_starttag(self, tag: str, attrs: list) -> None:
        attributes = dict(attrs)
        element_id = attributes.get("id")
        if element_id:
            if element_id in self.ids:
                self.duplicates.append(element_id)
            self.ids.add(element_id)
        if tag in ("a", "link", "img", "script"):
            for key in ("href", "src"):
                if attributes.get(key):
                    self.links.append(attributes[key])


def load(root: str) -> dict[str, tuple[Page, str]]:
    pages = {}
    for directory, _, files in os.walk(root):
        for name in files:
            if name.endswith(".html"):
                path = os.path.join(directory, name)
                with open(path, encoding="utf-8") as f:
                    text = f.read()
                page = Page()
                page.feed(text)
                pages[path] = (page, text)
    return pages


def target_of(root: str, page: str, link: str) -> tuple[str, str]:
    url = urllib.parse.urlparse(link)
    if url.path.startswith("/"):
        target = os.path.join(root, url.path.lstrip("/"))
    elif url.path:
        target = os.path.normpath(os.path.join(os.path.dirname(page), url.path))
    else:
        target = page
    if os.path.isdir(target):
        target = os.path.join(target, "index.html")
    return target, url.fragment


def check(root: str) -> list[str]:
    pages = load(root)
    problems = []
    for path, (page, text) in pages.items():
        name = os.path.relpath(path, root)
        problems += [f"duplicate id {d} in {name}" for d in page.duplicates]
        for link in page.links:
            if link.startswith(("http:", "https:", "data:", "mailto:")):
                continue
            target, fragment = target_of(root, path, link)
            if not os.path.exists(target):
                problems.append(f"broken link in {name}: {link}")
            elif fragment and target in pages and fragment not in pages[target][0].ids:
                problems.append(f"missing anchor in {name}: {link}")
        if 'http-equiv="refresh"' in text or "location.replace" in text:
            continue
        if re.search(r"<h1>\s*</h1>", text):
            problems.append(f"page without a title: {name}")
        if re.search(r'<a href="[^"]*"( aria-current="page")?>\s*</a>', text):
            problems.append(f"empty navigation link in {name}")
        if os.path.basename(path) not in ARTIFACT_EXEMPT:
            for pattern in ARTIFACTS:
                if re.search(pattern, text):
                    problems.append(f"unrendered markup {pattern!r} in {name}")
    return problems


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__, file=sys.stderr)
        return 2
    try:
        problems = check(sys.argv[1])
    except OSError as error:
        print(f"cannot read the site: {error}", file=sys.stderr)
        return 2
    for problem in problems:
        print(problem)
    print(f"{len(problems)} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
