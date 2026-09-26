"""Drawing primitives for the documentation diagrams in docs/assets/.

Every diagram is a self-contained dark panel in the colosseum palette, so it
reads the same in GitHub's light and dark themes. Text is real SVG text (not
paths), wrapped here by an approximate character budget.
"""
from __future__ import annotations

import html

from icons import sprite

BG = "#10131a"
CARD = "#1e2330"
RAISED = "#181c26"
INK = "#e9e4d8"
DIM = "#a2a7b5"
EMBER = "#e8590c"
BRIGHT = "#ff8a3d"
GOLD = "#f4c542"
GRAY = "#5b6270"
DANGER = "#c0392b"
BORDER = "#3a3f4d"
SANS = "ui-sans-serif, -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif"
MONO = "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace"


def esc(text: str) -> str:
    return html.escape(text, quote=True)


def wrap(text: str, width_px: float, size: float, mono: bool = False) -> list[str]:
    """Greedy word wrap using an average glyph width for the font stack."""
    per_char = size * (0.61 if mono else 0.53)
    budget = max(1, int(width_px / per_char))
    lines: list[str] = []
    for paragraph in text.split("\n"):
        line = ""
        for word in paragraph.split(" "):
            candidate = f"{line} {word}".strip()
            if len(candidate) <= budget or not line:
                line = candidate
            else:
                lines.append(line)
                line = word
        lines.append(line)
    return lines


class Canvas:
    def __init__(self, width: int, height: int, title: str, desc: str) -> None:
        self.width = width
        self.height = height
        self.parts = [
            f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {width} {height}" '
            f'width="{width}" height="{height}" role="img" aria-labelledby="t d">',
            f'<title id="t">{esc(title)}</title>',
            f'<desc id="d">{esc(desc)}</desc>',
            f'<rect width="{width}" height="{height}" rx="18" fill="{BG}"/>',
            f'<rect x="1" y="1" width="{width - 2}" height="{height - 2}" rx="17" fill="none" stroke="{BORDER}"/>',
        ]

    def add(self, fragment: str) -> None:
        self.parts.append(fragment)

    def heading(self, title: str, subtitle: str = "", icon: str | None = None) -> None:
        x = 40
        if icon:
            self.add(sprite(icon, 40, 30, 40))
            x = 94
        self.text(x, 60, title, size=27, weight=700, fill=INK)
        if subtitle:
            self.text(x, 90, subtitle, size=16, fill=DIM)

    def text(self, x: float, y: float, content: str, *, size: float = 15, fill: str = DIM,
             weight: int = 400, anchor: str = "start", mono: bool = False,
             italic: bool = False, spacing: float = 0) -> None:
        family = MONO if mono else SANS
        extra = ' font-style="italic"' if italic else ""
        if spacing:
            extra += f' letter-spacing="{spacing}"'
        self.add(
            f'<text x="{x:.1f}" y="{y:.1f}" text-anchor="{anchor}" font-family="{family}" '
            f'font-size="{size}" font-weight="{weight}" fill="{fill}"{extra}>{esc(content)}</text>'
        )

    def lines(self, x: float, y: float, lines: list[str], *, size: float = 15,
              fill: str = DIM, leading: float | None = None, mono: bool = False,
              anchor: str = "start") -> float:
        """Draws pre-wrapped lines; returns the baseline after the last one."""
        leading = leading or size * 1.45
        for index, line in enumerate(lines):
            self.text(x, y + index * leading, line, size=size, fill=fill, mono=mono, anchor=anchor)
        return y + (len(lines) - 1) * leading

    def paragraph(self, x: float, y: float, content: str, width: float, *, size: float = 15,
                  fill: str = DIM, mono: bool = False, anchor: str = "start") -> float:
        return self.lines(x, y, wrap(content, width, size, mono), size=size, fill=fill,
                          mono=mono, anchor=anchor)

    def box(self, x: float, y: float, w: float, h: float, *, fill: str = CARD,
            stroke: str = BORDER, width: float = 1, radius: float = 12, dash: str = "") -> None:
        dashed = f' stroke-dasharray="{dash}"' if dash else ""
        self.add(
            f'<rect x="{x:.1f}" y="{y:.1f}" width="{w:.1f}" height="{h:.1f}" rx="{radius}" '
            f'fill="{fill}" stroke="{stroke}" stroke-width="{width}"{dashed}/>'
        )

    def badge(self, cx: float, cy: float, label: str, *, fill: str = EMBER) -> None:
        self.add(f'<circle cx="{cx:.1f}" cy="{cy:.1f}" r="15" fill="{fill}"/>')
        self.text(cx, cy + 5.5, label, size=15, weight=700, fill=BG, anchor="middle")

    def icon(self, name: str, x: float, y: float, size: float = 40) -> None:
        self.add(sprite(name, x, y, size))

    def arrow(self, points: list[tuple[float, float]], *, color: str = BRIGHT,
              dash: str = "", width: float = 2.5, head: bool = True) -> None:
        path = "M" + " L".join(f"{x:.1f} {y:.1f}" for x, y in points)
        dashed = f' stroke-dasharray="{dash}"' if dash else ""
        self.add(f'<path d="{path}" fill="none" stroke="{color}" stroke-width="{width}"'
                 f' stroke-linejoin="round"{dashed}/>')
        if head and len(points) >= 2:
            (x1, y1), (x2, y2) = points[-2], points[-1]
            dx, dy = x2 - x1, y2 - y1
            length = max((dx * dx + dy * dy) ** 0.5, 1e-6)
            ux, uy = dx / length, dy / length
            px, py = -uy, ux
            left = (x2 - ux * 10 + px * 6, y2 - uy * 10 + py * 6)
            right = (x2 - ux * 10 - px * 6, y2 - uy * 10 - py * 6)
            self.add(f'<path d="M{left[0]:.1f} {left[1]:.1f} L{x2:.1f} {y2:.1f} '
                     f'L{right[0]:.1f} {right[1]:.1f}" fill="none" stroke="{color}" '
                     f'stroke-width="{width}" stroke-linejoin="round" stroke-linecap="round"/>')

    def svg(self) -> str:
        return "\n".join(self.parts + ["</svg>"]) + "\n"


def term_card(canvas: Canvas, x: float, y: float, w: float, h: float, icon: str, word: str,
              analogy: str, meaning: str, *, accent: str = EMBER, size: float = 15) -> None:
    """A glossary card: icon, word, 'like ...' analogy, and a plain meaning."""
    canvas.box(x, y, w, h)
    canvas.add(f'<rect x="{x:.1f}" y="{y + 14:.1f}" width="4" height="{h - 28:.1f}" rx="2" fill="{accent}"/>')
    canvas.icon(icon, x + 18, y + 18, 44)
    canvas.text(x + 76, y + 36, word, size=20, weight=700, fill=INK)
    canvas.text(x + 76, y + 59, f"like {analogy}", size=14, fill=GOLD, italic=True)
    canvas.paragraph(x + 76, y + 88, meaning, w - 96, size=size)


def step_card(canvas: Canvas, x: float, y: float, w: float, h: float, number: str, icon: str,
              title: str, label: str, body: str, *, highlight: bool = False,
              badge_fill: str = EMBER) -> None:
    """A numbered step: badge and icon on top, then title, label, and body."""
    canvas.box(x, y, w, h, stroke=EMBER if highlight else BORDER, width=2 if highlight else 1)
    canvas.badge(x + 34, y + 36, number, fill=badge_fill)
    canvas.icon(icon, x + w - 64, y + 16, 44)
    canvas.text(x + 22, y + 92, title, size=19 if w >= 240 else 18, weight=700, fill=INK)
    top = y + 118
    if label:
        canvas.text(x + 22, top, label, size=12.5, weight=700, fill=GOLD, spacing=1.6)
        top += 30
    canvas.paragraph(x + 22, top, body, w - 40, size=15 if w >= 240 else 14)
