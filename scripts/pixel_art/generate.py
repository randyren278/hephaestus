#!/usr/bin/env python3
"""Reproducible pixel-art asset generator for Hephaestus.

Renders small block-glyph sprites as SVG pixel grids using the same palette
as the web console (apps/hephaestus-web/src/web/styles.css) and the README
colosseum hero, so TUI/web/README visuals stay in one family. Re-run this
script to regenerate docs/assets/pixel/*.svg after editing a sprite grid
below — nothing here is hand-drawn or binary, so the assets are diffable
and reproducible.

Usage: python3 scripts/pixel_art/generate.py
"""
from __future__ import annotations

import pathlib

PALETTE = {
    ".": None,               # transparent
    "b": "#10131a",          # bg
    "r": "#181c26",          # bg-raised
    "c": "#1e2330",          # bg-card
    "i": "#e9e4d8",          # ink
    "d": "#a2a7b5",          # ink-dim
    "e": "#e8590c",          # ember
    "E": "#ff8a3d",          # ember-bright
    "g": "#f4c542",          # gold
    "y": "#5b6270",          # gray
    "n": "#c0392b",          # danger
    "o": "#3a3f4d",          # border
}

CELL = 8  # px per pixel-grid cell at 1x; SVG viewBox stays in grid units


def render_svg(grid: list[str], *, cell: int = CELL) -> str:
    rows = [row for row in grid if row]
    height = len(rows)
    width = max(len(row) for row in rows)
    parts: list[str] = []
    for y, row in enumerate(rows):
        for x, ch in enumerate(row):
            color = PALETTE.get(ch)
            if not color:
                continue
            parts.append(
                f'<rect x="{x}" y="{y}" width="1" height="1" fill="{color}"/>'
            )
    body = "".join(parts)
    return (
        f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
        f'width="{width * cell}" height="{height * cell}" '
        f'shape-rendering="crispEdges" role="img">{body}</svg>\n'
    )


# --- Sprites -----------------------------------------------------------
# Each string is one row; each character is one PALETTE key. Grids are kept
# small (block-glyph style) to match the TUI's 80x24 aesthetic.

CHAMPION_ICON = [
    "....gg....",
    "...gEEg...",
    "..gEEEEg..",
    "..gEEEEg..",
    "...gEEg...",
    "....gg....",
    "...iggi...",
    "..iiggii..",
    "..iiiiii..",
    ".iiiiiiii.",
]

GENE_ICON = [
    "..e....y..",
    "...e..y...",
    "....ey....",
    "...y..e...",
    "..y....e..",
    "..e....y..",
    "...e..y...",
    "....ey....",
    "...y..e...",
    "..y....e..",
]

CANARY_ICON = [
    "....g.....",
    "...ggg....",
    "..ggEgg...",
    ".ggEEEgg..",
    "ggEEEEEgg.",
    "..gg.gg...",
    "...g.g....",
    "..d...d...",
]

# 64x14 splash banner: colosseum arch silhouette + torches, echoing the
# README hero (torch-lit colosseum, orange vs graphite crests).
def _banner() -> list[str]:
    width = 64
    rows = ["b" * width for _ in range(14)]
    grid = [list(row) for row in rows]

    def set_px(x: int, y: int, ch: str) -> None:
        if 0 <= x < width and 0 <= y < len(grid):
            grid[y][x] = ch

    # ground line
    for x in range(width):
        set_px(x, 12, "o")
        set_px(x, 13, "b")

    # arches across the colosseum silhouette
    for arch_x in range(4, width - 4, 8):
        for y in range(6, 12):
            set_px(arch_x, y, "y")
            set_px(arch_x + 4, y, "y")
        for dx in range(1, 4):
            set_px(arch_x + dx, 6, "y")

    # two torches (ember vs graphite crest, echoing the hero image)
    for tx, flame in ((10, "E"), (width - 12, "d")):
        for y in range(2, 7):
            set_px(tx, y, "o")
        set_px(tx - 1, 1, flame)
        set_px(tx, 0, flame)
        set_px(tx + 1, 1, flame)

    # gold title bar accent
    for x in range(width):
        set_px(x, 9 if x % 2 == 0 else 9, "b")

    return ["".join(row) for row in grid]


BANNER = _banner()

WEB_HEADER_CREST = [
    ".....gg.....",
    "....gEEg....",
    "...gEEEEg...",
    "..gEEEEEEg..",
    "..gEyEEyEg..",
    "...gEEEEg...",
    "....gEEg.....",
    ".....gg.....",
]


SPRITES = {
    "champion-icon.svg": CHAMPION_ICON,
    "gene-icon.svg": GENE_ICON,
    "canary-icon.svg": CANARY_ICON,
    "tui-banner.svg": BANNER,
    "web-header-crest.svg": WEB_HEADER_CREST,
}


def main() -> None:
    out_dir = pathlib.Path(__file__).resolve().parents[2] / "docs" / "assets" / "pixel"
    out_dir.mkdir(parents=True, exist_ok=True)
    for name, grid in SPRITES.items():
        (out_dir / name).write_text(render_svg(grid), encoding="utf-8")
        print(f"wrote {out_dir / name}")


if __name__ == "__main__":
    main()
