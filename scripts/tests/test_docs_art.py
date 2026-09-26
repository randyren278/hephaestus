"""Contracts for scripts/docs_art: well-formed, unique icons and fresh diagrams."""
from __future__ import annotations

import pathlib
import re
import sys
import unittest
import xml.etree.ElementTree as ElementTree

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/docs_art"))

import diagrams  # noqa: E402
from icons import ICONS, PALETTE  # noqa: E402


class IconTests(unittest.TestCase):
    def test_every_icon_is_a_12_by_12_grid_of_palette_keys(self) -> None:
        for name, grid in ICONS.items():
            with self.subTest(icon=name):
                self.assertEqual(len(grid), 12)
                for row in grid:
                    self.assertEqual(len(row), 12, row)
                    self.assertTrue(set(row) <= set(PALETTE), row)

    def test_no_two_concepts_share_an_icon(self) -> None:
        seen: dict[tuple[str, ...], str] = {}
        for name, grid in ICONS.items():
            key = tuple(grid)
            self.assertNotIn(key, seen, f"{name} duplicates {seen.get(key)}")
            seen[key] = name

    def test_every_glossary_term_has_its_own_icon(self) -> None:
        icons = [icon for group in (diagrams.BASICS, diagrams.IMPROVING, diagrams.SAFETY)
                 for icon, *_ in group]
        self.assertEqual(len(icons), len(set(icons)))
        self.assertTrue(set(icons) <= set(ICONS))


class DiagramTests(unittest.TestCase):
    def test_committed_diagrams_match_the_generator(self) -> None:
        for name, build in diagrams.DIAGRAMS.items():
            with self.subTest(diagram=name):
                committed = (ROOT / "docs/assets" / name).read_text(encoding="utf-8")
                self.assertEqual(committed, build(),
                                 "stale: run python3 scripts/docs_art/diagrams.py")

    def test_diagrams_are_accessible_svg_without_repeated_icons(self) -> None:
        for name, build in diagrams.DIAGRAMS.items():
            with self.subTest(diagram=name):
                svg = build()
                root = ElementTree.fromstring(svg)
                namespace = "{http://www.w3.org/2000/svg}"
                self.assertTrue(root.find(f"{namespace}title").text)
                self.assertTrue(root.find(f"{namespace}desc").text)
                placed = re.findall(r'<g transform="translate\([^)]*\) scale\([^)]*\)" '
                                    r'shape-rendering="crispEdges">(.*?)</g>', svg)
                self.assertEqual(len(placed), len(set(placed)),
                                 "an icon appears twice in one diagram")


if __name__ == "__main__":
    unittest.main()
