#!/usr/bin/env python3
"""Regenerates every documentation diagram in docs/assets/.

Usage: python3 scripts/docs_art/diagrams.py

Each function below returns one SVG. Edit the words here, re-run the
script, and commit the regenerated files; nothing in docs/assets/*.svg
(other than the pixel/ folder, which scripts/pixel_art/generate.py owns)
is edited by hand.
"""
from __future__ import annotations

import math
import pathlib
import sys

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))

from render import (  # noqa: E402
    BG, BORDER, BRIGHT, CARD, DANGER, DIM, EMBER, GOLD, GRAY, INK, RAISED,
    Canvas, step_card, term_card, wrap,
)

ASSETS = pathlib.Path(__file__).resolve().parents[2] / "docs" / "assets"

# The glossary, in one place: (icon, word, analogy, plain meaning).
BASICS = [
    ("genome", "Genome", "a recipe card sealed in plastic",
     "One exact version of an agent: prompt, model, permissions. Any change makes a new one."),
    ("agent", "Agent", "the dish cooked from the recipe",
     "A Genome actually running on a task."),
    ("world", "World", "the exam and the exam-hall rules",
     "The tasks, limits, judge, and rules an agent is measured against."),
    ("law", "Law", "a rule carved in stone",
     "A World rule no agent can change, like “never read the judge.”"),
    ("arena", "Arena", "the exam hall",
     "Two Genomes do identical tasks, each sealed in its own sandbox."),
    ("evaluator", "Evaluator", "a judge behind a curtain",
     "Scores every run. Agents never see it or the answers."),
    ("receipt", "Receipt", "a signed scorecard",
     "Proof behind one decision: what ran, what it scored, and why."),
    ("ledger", "Ledger", "a notebook with glued-in pages",
     "The permanent record. Each entry is chained to the last, so edits show."),
    ("champion", "Champion", "the title holder",
     "The version trusted as best for a World, until a challenger wins fairly."),
    ("freeze", "Freeze", "an emergency brake, on by default",
     "Hephaestus starts paused. Only you can release it, and that is recorded too."),
    ("daemon", "Daemon", "the smith at the anvil",
     "hephaestusd: does the work, and is the only writer to the Ledger."),
]
IMPROVING = [
    ("mutation", "Mutation", "one edit to the recipe",
     "A single proposed change, with its reason, waiting to be tested."),
    ("descendant", "Descendant", "a new sprout",
     "A Genome made from one or more parents. It records who they were."),
    ("generation", "Generation", "a step down the stairs",
     "How many steps a Genome is from its first ancestor."),
    ("lineage", "Lineage", "the family tree",
     "Every related version, and who came from whom."),
    ("forge", "Forge", "the hammer and the workbench",
     "Studies failures, proposes a Mutation, and builds the new version."),
    ("promotion", "Promotion", "stepping onto the podium",
     "Replacing the Champion, only when the evidence clears the World's bar."),
    ("rollback", "Rollback", "turning back the hourglass",
     "Returning to an earlier Champion, which can always be rebuilt exactly."),
    ("gene", "Gene", "a trick that works in other kitchens",
     "A change that helped in more places than where it was first tried."),
    ("gene-bank", "Gene Bank", "a chest of proven tricks",
     "The collection of Genes and the evidence for each one."),
    ("species", "Species", "a key cut for one lock",
     "A branch of the family that measurably does best at one kind of task."),
]
SAFETY = [
    ("drift", "Drift", "the wind changing",
     "The tasks or conditions have shifted, so old scores may not hold."),
    ("canary", "Canary", "the canary in the coal mine",
     "Roll a new Champion out in stages, and back out automatically if it gets worse."),
    ("sandbox", "Sandbox", "a locked crate",
     "The macOS isolation every run happens in, cut off from the judge and the record."),
]
README_TERMS = ["genome", "world", "law", "arena", "evaluator", "ledger", "receipt",
                "champion", "freeze", "daemon"]


def describe_terms(entries) -> str:
    return " ".join(f"{word}: like {analogy}. {meaning}" for _, word, analogy, meaning in entries)


def how_it_works() -> str:
    steps = [
        ("genome", "Write the agent", "GENOME",
         "Your agent's recipe: its prompt, model, and permissions, sealed with a fingerprint so it can never change behind your back."),
        ("world", "Set the rules", "WORLD",
         "The exam your agent sits: the tasks, the time and cost limits, and what agents are never allowed to touch."),
        ("mutation", "Try a change", "MUTATION",
         "Edit the recipe. The new version gets its own fingerprint and remembers exactly which version it came from."),
        ("arena", "Fair fight", "ARENA",
         "Old and new do the same tasks, same starting point, same budget, each locked in its own sandbox."),
        ("evaluator", "Sealed judge", "EVALUATOR",
         "Scores both runs. The agents can never read the judge or the answers, so they can't game the test."),
        ("ledger", "Write it down", "LEDGER",
         "The verdict becomes a receipt in a record nobody can quietly edit. A clear winner can become Champion."),
    ]
    c = Canvas(960, 760, "How Hephaestus decides whether a change made your agent better",
               "Six steps. " + " ".join(f"{i + 1}, {t.lower()}: {b}" for i, (_, t, _, b) in enumerate(steps)))
    c.heading("How Hephaestus decides if a change made your agent better",
              "Two versions. One fair test. One judge they can't see. One record that can't be rewritten.")
    xs, ys, w, h = [40, 344, 648], [124, 446], 272, 266
    for i, (icon, title, label, body) in enumerate(steps):
        step_card(c, xs[i % 3], ys[i // 3], w, h, str(i + 1), icon, title, label, body, highlight=i == 5)
    for row in range(2):
        cy = ys[row] + h / 2
        c.arrow([(xs[0] + w + 4, cy), (xs[1] - 4, cy)])
        c.arrow([(xs[1] + w + 4, cy), (xs[2] - 4, cy)])
    mid = (ys[0] + h + ys[1]) / 2
    c.arrow([(xs[2] + w / 2, ys[0] + h + 4), (xs[2] + w / 2, mid), (xs[0] + w / 2, mid), (xs[0] + w / 2, ys[1] - 4)], dash="6 6")
    c.text(480, mid - 8, "then Hephaestus measures it", size=13.5, anchor="middle")
    return c.svg()


def glossary_cards(entries, columns: int, width: int, top: int, card_h: int, gap: int,
                   c: Canvas, size: float = 15) -> int:
    card_w = (width - 80 - gap * (columns - 1)) / columns
    for index, (icon, word, analogy, meaning) in enumerate(entries):
        x = 40 + (index % columns) * (card_w + gap)
        y = top + (index // columns) * (card_h + gap)
        term_card(c, x, y, card_w, card_h, icon, word, analogy, meaning, size=size)
    rows = math.ceil(len(entries) / columns)
    return top + rows * (card_h + gap)


def glossary_readme() -> str:
    lookup = {icon: (icon, word, analogy, meaning) for icon, word, analogy, meaning in BASICS}
    entries = [lookup[name] for name in README_TERMS]
    height = 118 + 5 * (146 + 16) + 50
    c = Canvas(960, height, "The words you'll see in Hephaestus", describe_terms(entries))
    c.heading("Ten words, in plain English", "Each one means exactly one thing, everywhere in Hephaestus.")
    bottom = glossary_cards(entries, 2, 960, 118, 146, 16, c)
    c.text(40, bottom + 20, "More words (Forge, Gene, Lineage, Drift, Canary) are in the full glossary: docs/TERMINOLOGY.md", size=14)
    return c.svg()


def glossary_full() -> str:
    sections = [
        ("The basics", "What you need for the tour and the quickstart.", BASICS),
        ("Improving agents over time", "What matters once Hephaestus proposes and tests changes itself.", IMPROVING),
        ("Safety and rollouts", "How changes reach real use without risking the current Champion.", SAFETY),
    ]
    card_h, gap, columns = 146, 14, 2
    height = 112
    for _, _, entries in sections:
        height += 70 + math.ceil(len(entries) / columns) * (card_h + gap)
    height += 20
    every = [entry for _, _, entries in sections for entry in entries]
    c = Canvas(960, height, "The Hephaestus glossary", describe_terms(every))
    c.heading("The Hephaestus glossary", "Every word, what it is like, and what it means. One word, one meaning, everywhere.")
    y = 112
    for title, subtitle, entries in sections:
        c.text(40, y + 30, title, size=20, weight=700, fill=GOLD)
        c.text(40, y + 54, subtitle, size=14.5)
        y = glossary_cards(entries, columns, 960, y + 70, card_h, gap, c, size=15)
    return c.svg()


def under_the_hood() -> str:
    c = Canvas(960, 470, "Under the hood of Hephaestus",
               "You drive Hephaestus from heph, the CLI, or a console. Every request goes to hephaestusd, the only program allowed to write the record. It runs the old and new versions in the Arena, each in its own sandbox; a sealed judge scores both and sends back signed scores; the daemon writes them to the Ledger; and replaying the Ledger must reproduce the live state exactly.")
    c.heading("Under the hood", "One writer, one record, and a locked box for every agent run.")
    # operator
    c.box(40, 150, 190, 150)
    c.icon("operator", 111, 162, 48)
    c.text(135, 232, "You", size=19, weight=700, fill=INK, anchor="middle")
    c.lines(135, 256, ["heph, the CLI,", "or a console"], size=14, anchor="middle")
    # daemon
    c.box(300, 150, 200, 150, stroke=GOLD, width=2)
    c.icon("daemon", 380, 166, 40)
    c.text(400, 232, "hephaestusd", size=19, weight=700, fill=INK, anchor="middle")
    c.lines(400, 256, ["the only program that", "writes the record"], size=14, anchor="middle")
    # arena with two sandboxes
    c.box(570, 124, 350, 200, fill=RAISED)
    c.icon("arena", 590, 138, 34)
    c.text(634, 162, "Arena", size=19, weight=700, fill=INK)
    for i, (label, color) in enumerate((("old version", GRAY), ("new version", EMBER))):
        x = 592 + i * 162
        c.box(x, 184, 146, 76, stroke=color, width=2, radius=8)
        c.text(x + 73, 214, label, size=15, weight=700, fill=INK, anchor="middle")
        c.text(x + 73, 238, "in its own sandbox", size=13, anchor="middle")
    c.icon("sandbox", 732, 272, 28)
    c.text(766, 292, "locked, no peeking", size=13)
    # judge and ledger
    c.box(640, 364, 280, 76)
    c.icon("evaluator", 656, 382, 40)
    c.text(708, 396, "Sealed judge", size=17, weight=700, fill=INK)
    c.text(708, 419, "scores both runs, nothing more", size=13.5)
    c.box(300, 364, 200, 76)
    c.icon("ledger", 316, 382, 40)
    c.text(368, 396, "Ledger", size=17, weight=700, fill=INK)
    c.text(368, 419, "the chained record", size=13.5)
    # flows
    c.arrow([(230, 225), (296, 225)])
    c.text(263, 212, "asks", size=13, anchor="middle")
    c.arrow([(500, 200), (566, 200)])
    c.text(533, 187, "runs", size=13, anchor="middle")
    c.arrow([(780, 324), (780, 360)])
    c.arrow([(640, 402), (560, 402), (560, 272), (504, 272)])
    c.text(572, 342, "scores", size=13)
    c.arrow([(400, 300), (400, 360)])
    c.text(410, 336, "signs + writes", size=13)
    c.arrow([(300, 402), (135, 402), (135, 304)], dash="6 6")
    c.text(150, 392, "replay must match", size=13)
    return c.svg()


def senate() -> str:
    desc = ("A semicircular chamber of senators around a clerk. You ask a question and pick a size "
            "from 3 to 15 senators. Every senator gives an opening view, the clerk drafts one answer, "
            "and each round senators vote agree, amend, or dissent and sharpen their views. When nobody "
            "dissents and most agree, the draft passes; otherwise the clerk redrafts, up to a round limit. "
            "You get one answer, the points of agreement, and any dissent credited to whoever raised it.")
    c = Canvas(960, 520, "How the Senate debates a question", desc)
    c.heading("The Senate: one question, many minds, one answer",
              "Simulated perspectives argue it out, round by round, until they agree or the clock runs out.",
              icon="senate")
    cx, cy = 255, 400
    palette = [BRIGHT, GOLD, EMBER, GRAY]
    for ring, (radius, seats) in enumerate([(112, 5), (162, 7), (212, 9)]):
        c.add(f'<path d="M{cx - radius} {cy} A{radius} {radius} 0 0 1 {cx + radius} {cy}" fill="none" '
              f'stroke="{BORDER}" stroke-width="1.5" stroke-dasharray="3 6"/>')
        for k in range(seats):
            angle = math.pi * (k + 0.5) / seats
            x, y = cx - radius * math.cos(angle), cy - radius * math.sin(angle)
            color = DANGER if (ring == 2 and k == 1) else palette[(ring + k) % 4]
            c.add(f'<circle cx="{x:.1f}" cy="{y - 9:.1f}" r="7.5" fill="{color}"/>')
            c.add(f'<path d="M{x - 11:.1f} {y + 8:.1f} q11 -18 22 0 z" fill="{color}"/>')
    c.box(cx - 55, cy - 34, 110, 40, stroke=GOLD, width=1.5, radius=6)
    c.text(cx, cy - 9, "the clerk", size=14, weight=700, fill=GOLD, anchor="middle")
    c.text(cx, cy + 34, "S 3 · M 5 · L 9 · XL 15 senators", size=13, anchor="middle")
    c.text(cx, cy + 56, "one red seat still dissents: its objection is credited in the answer", size=12, anchor="middle")
    steps = [
        ("Ask", "Pick a size, then ask anything: an idea or a draft."),
        ("Openings", "Every senator answers from their own point of view."),
        ("Draft", "A neutral clerk merges the views into one answer."),
        ("Vote", "Each senator votes agree, amend, or dissent."),
        ("Result", "One answer, the agreed points, and credited dissent."),
    ]
    x0, y0 = 500, 128
    for i, (title, body) in enumerate(steps):
        y = y0 + i * 72
        c.badge(x0 + 14, y + 14, str(i + 1), fill=EMBER if i < 4 else GOLD)
        if i < 4:
            c.add(f'<path d="M{x0 + 14} {y + 32} V{y + 66}" stroke="{BORDER}" stroke-width="2"/>')
        c.text(x0 + 42, y + 13, title, size=17, weight=700, fill=INK)
        c.paragraph(x0 + 42, y + 35, body, 250, size=14.5)
    lx = x0 + 300
    c.add(f'<path d="M{lx} {y0 + 230} C{lx + 40} {y0 + 230} {lx + 40} {y0 + 158} {lx + 6} {y0 + 158}" '
          f'fill="none" stroke="{BRIGHT}" stroke-width="2" stroke-dasharray="5 5"/>')
    c.add(f'<path d="M{lx + 14} {y0 + 152} L{lx + 4} {y0 + 158} L{lx + 14} {y0 + 164}" fill="none" stroke="{BRIGHT}" stroke-width="2"/>')
    c.text(lx + 44, y0 + 204, "no consensus?", size=12.5, fill=BRIGHT)
    c.text(lx + 44, y0 + 220, "redraft", size=12.5, fill=BRIGHT)
    return c.svg()


def flow(c: Canvas, steps, top: int, columns: int = 3, card_h: int = 200,
         highlight_last: bool = True, connector: str = "") -> int:
    """Numbered step cards in rows of `columns`, joined by arrows."""
    gap = 32
    width = (c.width - 80 - gap * (columns - 1)) / columns
    rows = math.ceil(len(steps) / columns)
    row_gap = 56
    for i, (icon, title, label, body) in enumerate(steps):
        x = 40 + (i % columns) * (width + gap)
        y = top + (i // columns) * (card_h + row_gap)
        step_card(c, x, y, width, card_h, str(i + 1), icon, title, label, body,
                  highlight=highlight_last and i == len(steps) - 1)
        if i % columns and True:
            cy = y + card_h / 2
            c.arrow([(x - gap + 4, cy), (x - 4, cy)])
    for row in range(rows - 1):
        y_bottom = top + row * (card_h + row_gap) + card_h
        mid = y_bottom + row_gap / 2
        last_x = 40 + (columns - 1) * (width + gap) + width / 2
        first_x = 40 + width / 2
        c.arrow([(last_x, y_bottom + 4), (last_x, mid), (first_x, mid), (first_x, y_bottom + row_gap - 4)], dash="6 6")
        if connector:
            c.text(c.width / 2, mid - 8, connector, size=13.5, anchor="middle")
    return top + rows * card_h + (rows - 1) * row_gap


def describe_steps(steps) -> str:
    return " ".join(f"{i + 1}, {title}: {body}" for i, (_, title, _, body) in enumerate(steps))


def tour() -> str:
    steps = [
        ("gate", "Welcome", "START",
         "What a Genome and a World are, and the one promise: every claim carries a receipt."),
        ("world", "Your first World", "SET UP",
         "Registers the bundled example World and two agents: a parent and a changed child."),
        ("freeze", "Unfreeze and run", "RUN",
         "Releases the brake Hephaestus starts with, then runs the parent once in a sandbox."),
        ("arena", "Measure in the Arena", "COMPARE",
         "Parent versus child on the same tasks: who won, by how much, and how sure we are."),
        ("ledger", "Prove it", "REPLAY",
         "Rebuilds everything from the record and checks it matches the live state exactly."),
        ("map", "Done", "FINISH",
         "Where every screen lives, and how to replay this tour with heph --tour."),
    ]
    c = Canvas(960, 690, "The six-step first-run tour", "The tour runs against the real daemon. " + describe_steps(steps))
    c.heading("The first-run tour, step by step", "Six short steps. Everything happens for real, and nothing is pre-recorded.")
    flow(c, steps, 124, card_h=244)
    return c.svg()


def cli_map() -> str:
    groups = [
        ("world", "Set up", ["world register", "genome register", "arena manifest", "artifact put", "verifier"]),
        ("agent", "Run once", ["run", "submit", "job status", "job kill", "evaluate"]),
        ("arena", "Compare", ["arena evaluate", "arena select", "arena invariants"]),
        ("forge", "Improve", ["forge analyze", "genome propose", "genome assess", "evolve start"]),
        ("champion", "Crown", ["champion seed", "champion promote", "champion rollback", "champion show"]),
        ("canary", "Roll out safely", ["drift record", "canary start", "canary advance", "canary live-check"]),
        ("gene", "Reuse what works", ["gene extract", "gene transfer", "gene record", "gene speciate"]),
        ("freeze", "Stay in control", ["status", "freeze / unfreeze", "kill --all", "replay", "daemon stop"]),
    ]
    c = Canvas(960, 640, "Map of the hephaestus commands",
               "The hephaestus commands grouped by job. " + " ".join(f"{t}: {', '.join(cmds)}." for _, t, cmds in groups))
    c.heading("Every command, grouped by what it's for", "Each one is typed after hephaestus, for example: hephaestus arena evaluate")
    columns, gap = 4, 18
    width = (960 - 80 - gap * (columns - 1)) / columns
    for i, (icon, title, cmds) in enumerate(groups):
        x = 40 + (i % columns) * (width + gap)
        y = 124 + (i // columns) * 252
        c.box(x, y, width, 234)
        c.icon(icon, x + 16, y + 16, 40)
        c.text(x + 66, y + 42, title, size=17, weight=700, fill=INK)
        for j, cmd in enumerate(cmds):
            c.text(x + 18, y + 90 + j * 27, cmd, size=13.5, fill=DIM, mono=True)
    return c.svg()


def parts_diagram(c: Canvas, center_icon: str, center_title: str, center_lines: list[str],
                  parts: list[tuple[str, str]], top: int, fingerprint: str) -> None:
    """Parts on the left feeding one sealed object on the right."""
    part_h, gap = 80, 10
    for i, (name, detail) in enumerate(parts):
        y = top + i * (part_h + gap)
        c.box(40, y, 430, part_h)
        c.add(f'<rect x="40" y="{y + 12}" width="4" height="{part_h - 24}" rx="2" fill="{GOLD}"/>')
        c.text(60, y + 27, name, size=16, weight=700, fill=INK)
        c.paragraph(60, y + 49, detail, 400, size=13.5)
        c.arrow([(474, y + part_h / 2), (560, top + len(parts) * (part_h + gap) / 2 - 6)], color=BORDER, width=1.5, head=False)
    mid = top + len(parts) * (part_h + gap) / 2 - 6
    c.box(560, mid - 120, 360, 240, stroke=EMBER, width=2)
    c.icon(center_icon, 700, mid - 100, 72)
    c.text(740, mid + 4, center_title, size=21, weight=700, fill=INK, anchor="middle")
    c.lines(740, mid + 30, center_lines, size=14, anchor="middle")
    c.box(580, mid + 72, 320, 32, fill=RAISED, radius=6)
    c.text(740, mid + 93, fingerprint, size=13, fill=GOLD, anchor="middle", mono=True)


def world_anatomy() -> str:
    parts = [
        ("Laws", "Rules no agent can change: no peeking at the judge, a cost cap."),
        ("Permission ceiling", "The most any agent here may do, such as writing files or using the network."),
        ("What may change", "Which parts the Forge may edit. Laws and the judge are never on the list."),
        ("Promotion bar", "How big and how certain a win must be before a new Champion is crowned."),
        ("Goals", "What counts as better, such as correctness."),
        ("Tasks and judge", "Visible tasks, sealed tasks, the judge program, and the key it trusts."),
    ]
    c = Canvas(960, 720, "What is inside a World",
               "A World bundles six parts: " + " ".join(f"{n}: {d}" for n, d in parts) + " Together they get one fingerprint. Results are comparable only when fingerprints match; changing any part makes a new World.")
    c.heading("What's inside a World", "Six parts, sealed together under one fingerprint.")
    parts_diagram(c, "world", "One World", ["change any part and you get", "a brand-new World"], parts, 116,
                  "hephaestus:world:<fingerprint>")
    c.text(740, 690, "Scores are only compared inside the same World.", size=14, fill=BRIGHT, anchor="middle")
    return c.svg()


def genome_anatomy() -> str:
    parts = [
        ("Name", "A label for people. It is not part of the fingerprint's ancestry."),
        ("Parents", "The exact earlier Genomes this one came from, if any."),
        ("Model", "Which model runs it. Today only the built-in test model executes."),
        ("Permissions", "May it write files? Use the network? Never more than its parents."),
        ("Prompt and files", "The instructions, stored by fingerprint so nobody can swap them."),
    ]
    c = Canvas(960, 800, "What is inside a Genome",
               "A Genome bundles a name, parents, model, permissions, and prompt, sealed under one fingerprint. Permissions can only shrink from World to parent to child.")
    c.heading("What's inside a Genome", "An agent's recipe, sealed. Change one letter and it is a different Genome.")
    parts_diagram(c, "genome", "One Genome", ["the same recipe always gets", "the same fingerprint"], parts, 116,
                  "hephaestus:genome:<fingerprint>")
    # nested ceilings
    y = 600
    c.text(40, y, "Permissions only ever shrink", size=18, weight=700, fill=GOLD)
    c.box(40, y + 16, 880, 160, fill=RAISED, stroke=GRAY)
    c.text(60, y + 42, "World ceiling: the most any agent here may do", size=14, fill=INK)
    c.box(80, y + 56, 800, 104, stroke=GRAY)
    c.text(100, y + 82, "Parent: equal to or less than the World", size=14, fill=INK)
    c.box(120, y + 96, 720, 50, stroke=EMBER, width=2)
    c.text(140, y + 127, "Child: equal to or less than every parent. A child that asks for more is refused.", size=14, fill=INK)
    return c.svg()


def run_sandbox() -> str:
    steps = [
        ("pin", "Pin everything", "BEFORE",
         "The exact Genome, World, task, code version, and budget are fixed before anything runs."),
        ("sandbox", "Lock it in", "SANDBOX",
         "A private copy of the code in a macOS sandbox. Only its own folder is writable; no network unless allowed."),
        ("agent", "Run it", "RUNNER",
         "The built-in test runner today. Claude Code and Codex runners exist and are opt-in."),
        ("receipt", "Sign the result", "AFTER",
         "Output and time are capped. The daemon signs what happened and files the evidence."),
    ]
    c = Canvas(960, 460, "What happens when an agent runs", describe_steps(steps) + " Hosts without a verified sandbox refuse to run agents at all.")
    c.heading("What happens when an agent runs", "Every run is pinned, locked in, capped, and signed.")
    flow(c, steps, 118, columns=4, card_h=290)
    c.text(40, 442, "No verified sandbox on this computer? Then Hephaestus refuses to run agents, rather than running them unprotected.", size=13.5, fill=BRIGHT)
    return c.svg()


def trace_pipeline() -> str:
    steps = [
        ("observe", "Observe", "WHAT HAPPENED",
         "Tool calls, files read, tests, costs, errors. Never hidden reasoning."),
        ("redact", "Scrub secrets", "REDACT",
         "Passwords, API keys, and tokens are removed before anything is saved."),
        ("fingerprint", "Cap and store", "BOUND",
         "Each record is size-limited, then stored under its own fingerprint."),
        ("ledger", "Note it", "RECEIPT",
         "The Ledger gets a short receipt pointing at the stored record."),
    ]
    c = Canvas(960, 400, "How a trace becomes a safe record", describe_steps(steps))
    c.heading("From what an agent did to a safe record", "Secrets are removed first, then everything is stored and receipted.")
    flow(c, steps, 118, columns=4, card_h=252)
    return c.svg()


def evolution_loop() -> str:
    steps = [
        ("arena", "Measure first", "BASELINE",
         "The current Champion runs against a fixed baseline, so every generation starts from fresh evidence."),
        ("forge", "Propose", "FORGE",
         "The Forge makes exactly one Mutation of the Champion and writes down why."),
        ("evaluator", "Test", "COMPARE",
         "Child versus Champion in the Arena, plus checks that nothing basic broke."),
        ("promotion", "Decide", "PROMOTE OR NOT",
         "The child takes the title only if the evidence clears the World's bar. Either way it stays on record."),
    ]
    c = Canvas(960, 480, "One generation of an evolve run",
               describe_steps(steps) + " Each generation costs two paired evaluations; the run stops at its budget or generation limit.")
    c.heading("One generation of hephaestus evolve", "Repeat until the generation limit or the budget runs out. Freeze stops it at any time.",
              icon="generation")
    bottom = flow(c, steps, 124, columns=4, card_h=290)
    c.text(40, bottom + 34, "Budget: each generation spends exactly two paired Arena evaluations.", size=14, fill=BRIGHT)
    return c.svg()


def canary_rollout() -> str:
    c = Canvas(960, 520, "How a new Champion is rolled out safely",
               "Drift is recorded when conditions change. A candidate is first tested in the shadow, then rolled out in stages of 5, 25, and 50 percent while the old Champion keeps its title. Healthy evidence at 50 percent promotes the candidate. A regression at any stage aborts automatically. After completion, a live check can roll back automatically.")
    c.heading("Rolling out a new Champion, one stage at a time", "The old Champion keeps the title until the very last stage passes.", icon="canary")
    # drift and shadow
    c.box(40, 116, 250, 134)
    c.icon("drift", 56, 132, 40)
    c.text(108, 156, "Something changed", size=16, weight=700, fill=INK)
    c.paragraph(56, 192, "Drift is recorded: slower, costlier, less correct, or new work.", 220, size=13.5)
    c.box(40, 276, 250, 134)
    c.icon("evaluator", 56, 292, 40)
    c.text(108, 316, "Shadow test", size=16, weight=700, fill=INK)
    c.paragraph(56, 352, "The candidate is tested next to the Champion, with no real traffic.", 220, size=13.5)
    c.arrow([(165, 250), (165, 272)])
    # stages
    stages = ["5%", "25%", "50%", "100%"]
    x0, y0, sw = 340, 250, 120
    c.arrow([(290, 343), (x0 - 6, 300)])
    for i, label in enumerate(stages):
        x = x0 + i * (sw + 22)
        final = i == 3
        c.box(x, y0, sw, 90, stroke=GOLD if final else EMBER, width=2)
        c.text(x + sw / 2, y0 + 44, label, size=26, weight=700, fill=GOLD if final else INK, anchor="middle")
        c.text(x + sw / 2, y0 + 70, "new Champion" if final else "of the rollout", size=12.5, anchor="middle")
        if i:
            c.arrow([(x - 20, y0 + 45), (x - 3, y0 + 45)])
    c.text(x0, y0 - 22, "Each stage needs fresh, healthy evidence to move on.", size=14, fill=INK)
    c.icon("promotion", x0 + 3 * (sw + 22) + sw / 2 - 20, y0 - 60, 40)
    # abort
    c.box(x0, 400, 404, 90, stroke=DANGER, width=2)
    c.icon("rollback", x0 + 16, 424, 40)
    c.text(x0 + 70, 432, "Worse at any stage? Automatic abort.", size=15, weight=700, fill=INK)
    c.text(x0 + 70, 456, "The old Champion was never replaced.", size=13.5)
    for i in range(3):
        x = x0 + i * (sw + 22) + sw / 2
        c.arrow([(x, y0 + 92), (x, 396)], color=DANGER, dash="4 5", width=2)
    c.box(772, 400, 148, 90)
    c.paragraph(786, 428, "After 100%, a live check can still roll back.", 124, size=13.5, fill=INK)
    return c.svg()


def gene_flow() -> str:
    steps = [
        ("promotion", "A win", "PROMOTION",
         "A child beat the Champion with at least 3 measured head-to-head trials."),
        ("gene", "Extract a Gene", "GENE",
         "The winning change is saved as a Gene, with its evidence attached."),
        ("lineage", "Try it elsewhere", "TRANSFER",
         "The same change is applied to a different family of agents and measured."),
        ("gene-bank", "Keep score", "GENE BANK",
         "Each trial is recorded as helped, no change, or hurt. Contradictions stay visible."),
        ("species", "Name a specialist", "SPECIES",
         "No harm anywhere, wins in 3 or more families, 3% or better on average: a new species."),
    ]
    c = Canvas(960, 690, "How the Gene Bank turns one win into reusable knowledge", describe_steps(steps))
    c.heading("From one lucky win to proven, reusable knowledge", "A Gene has to keep winning in new places before it counts.")
    flow(c, steps, 124, columns=3, card_h=244)
    return c.svg()


def meta_evolution() -> str:
    c = Canvas(960, 470, "How two improvement strategies are compared",
               "Two Evolver strategies each run the ordinary evolve engine over the same set of held-out agent families, starting from identical conditions each time. Hephaestus compares how many improvements each found and what each cost, and records a receipt with a confidence interval.")
    c.heading("Which way of improving agents works better?", "Two strategies, the same starting agents, a fair comparison.", icon="scales")
    for i, (label, color) in enumerate((("Strategy A", GRAY), ("Strategy B", EMBER))):
        y = 124 + i * 150
        c.box(40, y, 250, 120, stroke=color, width=2)
        c.text(60, y + 38, label, size=19, weight=700, fill=INK)
        c.paragraph(60, y + 66, "How many generations, and how to spend the budget.", 210, size=13.5)
    c.box(350, 124, 300, 270, fill=RAISED)
    c.icon("lineage", 370, 140, 40)
    c.text(422, 166, "Same agent families", size=17, weight=700, fill=INK)
    c.paragraph(370, 206, "Each strategy runs a full evolve on every family. Between runs, the Champion is reset, so both start from exactly the same place.", 262, size=14)
    c.arrow([(290, 184), (346, 220)])
    c.arrow([(290, 334), (346, 300)])
    c.box(710, 124, 210, 270, stroke=GOLD, width=2)
    c.icon("receipt", 726, 140, 40)
    c.text(778, 166, "Verdict", size=17, weight=700, fill=INK)
    c.paragraph(726, 206, "Which found more improvements, which cost less, and how sure we are.", 176, size=14)
    c.arrow([(650, 260), (706, 260)])
    c.text(40, 440, "Today only one kind of change exists, so most strategy settings are recorded but cannot yet make a difference.", size=13.5, fill=BRIGHT)
    return c.svg()


def mcp_gateway() -> str:
    c = Canvas(960, 440, "How an AI assistant talks to Hephaestus through the MCP gateway",
               "An AI assistant speaks MCP to the gateway. The gateway checks its per-client policy: 11 read-only tools, and 4 changing tools that need an extra grant. Allowed calls go through the same authenticated door the CLI uses, and the daemon still enforces every rule. Every call, allowed or denied, is written to the Ledger.")
    c.heading("Letting an AI assistant use Hephaestus", "Through the same front door as you, with a guest list, and every visit written down.")
    c.box(40, 140, 220, 150)
    c.icon("agent", 128, 154, 44)
    c.text(150, 226, "AI assistant", size=17, weight=700, fill=INK, anchor="middle")
    c.text(150, 250, "speaks MCP", size=13.5, anchor="middle")
    c.box(330, 120, 290, 190, stroke=GOLD, width=2)
    c.icon("guestlist", 346, 136, 40)
    c.text(398, 162, "The gateway", size=17, weight=700, fill=INK)
    c.lines(350, 204, ["11 read-only tools: look around", "4 changing tools: need an extra", "grant from you", "not on the list: refused"], size=14)
    c.box(690, 140, 230, 150)
    c.icon("daemon", 785, 154, 40)
    c.text(805, 226, "hephaestusd", size=17, weight=700, fill=INK, anchor="middle")
    c.lines(805, 250, ["still checks every rule", "(freeze, Laws, limits)"], size=13.5, anchor="middle")
    c.arrow([(260, 215), (326, 215)])
    c.arrow([(620, 215), (686, 215)])
    c.box(330, 350, 290, 64)
    c.icon("ledger", 346, 362, 40)
    c.text(398, 388, "Every call is recorded,", size=14, fill=INK)
    c.text(398, 406, "allowed or refused", size=14, fill=INK)
    c.arrow([(475, 310), (475, 346)])
    return c.svg()


def remote_workers() -> str:
    steps = [
        ("credential", "Mint a pass", "YOU",
         "You create an expiring credential for one worker. Its secret is shown to you once and never stored."),
        ("remote", "Lease one job", "WORKER",
         "The worker proves who it is and borrows one approved job. Revoked or expired passes are refused."),
        ("sandbox", "Run it, locked", "WORKER",
         "It runs the job in the same kind of sandbox the daemon would use, then hands back the raw output."),
        ("receipt", "Check and sign", "DAEMON",
         "Only the daemon holds the signing key. It checks the output, signs it, and records it."),
    ]
    c = Canvas(960, 470, "How remote workers run jobs",
               describe_steps(steps) + " A bad worker can at worst return a wrong answer for its one job; it can never forge a success.")
    c.heading("Running jobs on another machine", "Workers borrow the work. Only the daemon can sign the results.")
    flow(c, steps, 118, columns=4, card_h=296)
    c.text(40, 448, "A misbehaving worker can return a wrong answer for its one job, but never a forged success.", size=13.5, fill=BRIGHT)
    return c.svg()


def threat_model() -> str:
    c = Canvas(960, 560, "Who Hephaestus trusts and who it does not",
               "Trusted: you, the operator, and the daemon, the only writer, with the Ledger and the stored files. Untrusted: agents being tested and everything they produce, repositories, tool results, model output, and remote workers. Isolated: the judge and the sealed tasks. Every stored file is checked against its fingerprint on every read.")
    c.heading("Who is trusted, and who is not", "Agents are assumed to be hostile. The judge is assumed to leak if it can.")
    zones = [
        (40, "Trusted", GOLD, [("operator", "You, the operator", "Only you can unfreeze, promote, or kill."),
                               ("daemon", "The daemon", "The only thing that writes the record."),
                               ("ledger", "The Ledger", "Chained, and re-checked on every start.")]),
        (340, "Untrusted", DANGER, [("agent", "Agents under test", "And everything they output."),
                                   ("parcel", "Outside input", "Repos, tool results, model text."),
                                   ("remote", "Remote workers", "Can't sign; results are checked.")]),
        (640, "Walled off", GRAY, [("evaluator", "The judge", "Agents can never read it."),
                                  ("envelope", "Sealed tasks", "Only the judge sees these."),
                                  ("fingerprint", "Stored files", "Checked by fingerprint on every read.")]),
    ]
    for x, title, color, items in zones:
        c.box(x, 116, 280, 400, fill=RAISED, stroke=color, width=2)
        c.text(x + 20, 150, title, size=19, weight=700, fill=color if color != GRAY else INK)
        for j, (icon, name, detail) in enumerate(items):
            y = 172 + j * 112
            c.box(x + 16, y, 248, 98)
            c.icon(icon, x + 30, y + 14, 36)
            c.text(x + 78, y + 36, name, size=15, weight=700, fill=INK)
            c.paragraph(x + 32, y + 70, detail, 220, size=13)
    return c.svg()


def constitution() -> str:
    laws = [
        "A released Genome can never change.",
        "A child never gets more permissions than its parent.",
        "Agents can't touch the rules, the judge, the record, the budget, or the controls.",
        "A model may recommend a promotion, but never perform one.",
        "Every accepted change is recorded and can be replayed.",
        "Results from different Worlds are never compared as if they were the same.",
        "Freeze and kill always stay with the human operator.",
        "Anything malformed or unsupported is refused.",
    ]
    c = Canvas(960, 560, "The eight Laws of Hephaestus",
               "The eight Laws in plain English: " + " ".join(f"{i + 1}. {law}" for i, law in enumerate(laws)))
    c.heading("The eight Laws, in plain English", "No agent, model, or feature is allowed to break these.", icon="law")
    for i, law in enumerate(laws):
        x = 40 + (i % 2) * 448
        y = 120 + (i // 2) * 106
        c.box(x, y, 432, 92)
        c.badge(x + 32, y + 46, str(i + 1))
        c.paragraph(x + 62, y + 40, law, 350, size=15, fill=INK)
    return c.svg()


def crate_map() -> str:
    c = Canvas(960, 960, "How the Hephaestus codebase fits together",
               "Four layers. Front doors: the terminal and web consoles in apps/, and the programs you run, hephaestus and heph, plus the MCP gateway and remote worker. The engine: hephaestusd in hephaestus-control, the only writer. Capabilities: hephaestus-genome turns World and Genome files into sealed objects; hephaestus-runtime runs agents in locked sandboxes; hephaestus-experience records what happened with secrets scrubbed; hephaestus-arena runs fair paired comparisons. Foundation: hephaestus-ledger, the chained record and file store; hephaestus-core, the Laws and shared vocabulary. Standalone: hephaestus-senate, the debate tool, which needs none of the rest. Code only depends on its own layer or the layers below.")
    c.heading("How the codebase fits together", "Four layers, top to bottom, plus one tool that stands on its own.")
    layers = [
        ("FRONT DOORS", [("console", "Consoles", "apps/: the terminal and web screens."),
                         ("operator", "Programs you run", "hephaestus, heph, the MCP gateway, the remote worker.")]),
        ("THE ENGINE", [("daemon", "hephaestus-control", "hephaestusd: the daemon, its API, and the only writer to the record.")]),
        ("CAPABILITIES", [("genome", "hephaestus-genome", "Seals World and Genome files."),
                          ("sandbox", "hephaestus-runtime", "Runs agents in locked sandboxes."),
                          ("redact", "hephaestus-experience", "Records runs, secrets scrubbed."),
                          ("arena", "hephaestus-arena", "Fair, paired comparisons.")]),
        ("FOUNDATION", [("ledger", "hephaestus-ledger", "The chained record and fingerprinted file store."),
                        ("law", "hephaestus-core", "The Laws and the shared vocabulary.")]),
    ]
    y = 116
    for label, crates in layers:
        wide = len(crates) < 4
        h = 142 if wide else 190
        c.box(40, y, 880, h, fill=RAISED)
        c.text(58, y + 26, label, size=12.5, weight=700, fill=GOLD, spacing=1.6)
        inner_w = (880 - 36 - 14 * (len(crates) - 1)) / len(crates)
        for i, (icon, name, role) in enumerate(crates):
            x = 58 + i * (inner_w + 14)
            c.box(x, y + 40, inner_w, h - 56)
            if wide:
                c.icon(icon, x + 14, y + 56, 40)
                c.text(x + 68, y + 70, name, size=14, fill=INK, weight=700, mono=name.startswith("hephaestus-"))
                c.paragraph(x + 68, y + 94, role, inner_w - 84, size=13.5)
            else:
                c.icon(icon, x + 14, y + 52, 36)
                c.text(x + 14, y + 112, name, size=12.5, fill=INK, weight=700, mono=True)
                c.paragraph(x + 14, y + 136, role, inner_w - 28, size=13)
        if label != "FOUNDATION":
            c.arrow([(480, y + h + 2), (480, y + h + 16)], width=2)
        y += h + 18
    c.box(40, y + 6, 880, 96, stroke=GOLD, width=2, dash="6 5")
    c.icon("senate", 58, y + 26, 52)
    c.text(128, y + 44, "STANDALONE", size=12.5, weight=700, fill=GOLD, spacing=1.6)
    c.text(128, y + 68, "hephaestus-senate", size=14, fill=INK, weight=700, mono=True)
    c.text(128, y + 90, "The debate tool. It needs none of the layers above, so it installs on its own.", size=13.5)
    c.text(40, y + 132, "Code only depends on its own layer or the layers below it, never on a layer above.", size=13.5, fill=BRIGHT)
    return c.svg()

def node(c: Canvas, x: float, y: float, w: float, h: float, title: str, sub: str = "", *,
         stroke: str = BORDER, width: float = 1) -> None:
    """A labelled box: bold title and an optional wrapped subtitle."""
    c.box(x, y, w, h, stroke=stroke, width=width, radius=10)
    c.text(x + 14, y + 25, title, size=15, weight=700, fill=INK)
    if sub:
        c.paragraph(x + 14, y + 47, sub, w - 26, size=13)


def chip(c: Canvas, x: float, y: float, label: str, *, color: str = GOLD) -> None:
    """A small rounded tag, used for 'goes to another band' notes."""
    w = len(label) * 7.2 + 20
    c.box(x, y, w, 26, fill=RAISED, stroke=color, radius=13)
    c.text(x + w / 2, y + 17.5, label, size=12.5, weight=700, fill=color, anchor="middle")


def band(c: Canvas, y: float, h: float, number: str, title: str, icon: str) -> None:
    c.box(24, y, 912, h, fill=RAISED, radius=14)
    c.badge(52, y + 28, number, fill=GOLD)
    c.text(76, y + 34, title, size=18, weight=700, fill=GOLD)
    c.icon(icon, 880, y + 12, 36)


def system_map() -> str:
    desc = ("The full Hephaestus system in five parts. "
            "1, you and the daemon: the operator CLI talks over an owner-only socket that checks a token and schema version 1, to hephaestusd, the single writer. Candidates asking to unfreeze are always refused; only the operator resumes evolution. "
            "2, sealing Worlds and Genomes: versioned source files, stored artifacts, and the canonical vocabulary go through a fail-closed compiler that produces content-addressed Worlds and Genomes. Registration replay checks acyclic, same-World ancestry before the daemon trusts them. A child's permissions must be equal or narrower; a wider request is refused. "
            "3, running an agent: a pinned run spec gets a private Git worktree in a sandbox, and one of three runners executes it: the offline reference runner, Codex, or Claude. An evidence-required wrapper records redacted traces, and the daemon signs the terminal run result. "
            "4, measuring in the Arena: a trusted paired scheduler runs the parent and candidate as separate supervised processes; the Arena checks their signed results and asks a World-hashed isolated evaluator, which returns only totals, to score them. The World also supplies the canonical visible and sealed task manifests and the producer's public key. The Arena writes a verified evaluation event, which yields a candidate-safe visible summary and operator-only selection evidence that deterministic selection uses. "
            "5, the record: every change follows a declared lifecycle into a hash-linked event, stored in SQLite and a BLAKE3 content-addressed file store. Verified replay rebuilds the daemon's state from them and must match exactly.")
    c = Canvas(960, 1656, "The full Hephaestus system", desc)
    c.heading("The whole system, in five parts", "Every arrow is a real hand-off in the code. Gold tags say where a result goes next.")

    # 1. You and the daemon
    y = 112
    band(c, y, 206, "1", "You and the daemon", "operator")
    node(c, 48, y + 58, 200, 78, "Operator CLI", "hephaestus, heph, and the consoles")
    node(c, 300, y + 58, 250, 78, "Owner-only socket", "checks your token and schema v1")
    node(c, 602, y + 58, 310, 78, "hephaestusd", "the single writer; rebuilds its state from the record on start", stroke=GOLD, width=2)
    c.arrow([(248, y + 97), (296, y + 97)])
    c.arrow([(550, y + 97), (598, y + 97)])
    c.box(48, y + 150, 864, 40, fill=CARD, radius=8)
    c.text(64, y + 175, "Unfreeze: a candidate asking is always refused. Only the operator can resume evolution.", size=13.5, fill=INK)

    # 2. Sealing Worlds and Genomes
    y = 336
    band(c, y, 326, "2", "Sealing Worlds and Genomes", "genome")
    node(c, 48, y + 58, 190, 78, "Source files", "versioned JSON, YAML, or Markdown")
    node(c, 256, y + 58, 190, 78, "Stored files", "fingerprinted artifacts (CAS)")
    node(c, 464, y + 58, 190, 78, "Vocabulary", "one canonical meaning per word")
    node(c, 702, y + 58, 210, 78, "Fail-closed compiler", "anything unknown or unsafe is refused", stroke=EMBER, width=2)
    c.arrow([(654, y + 97), (698, y + 97)])
    for x in (143, 351, 559):
        c.arrow([(x, y + 136), (x, y + 150), (680, y + 150), (680, y + 110), (698, y + 110)], head=False, color=BORDER, width=1.5)
    node(c, 48, y + 184, 200, 78, "World", "content-addressed exam and rules")
    node(c, 296, y + 184, 220, 78, "Genome", "content-addressed agent, under a World")
    node(c, 564, y + 184, 348, 78, "Registration replay", "acyclic, same-World ancestry before the daemon trusts it")
    c.arrow([(807, y + 136), (807, y + 166), (148, y + 166), (148, y + 180)])
    c.arrow([(248, y + 223), (292, y + 223)])
    c.arrow([(516, y + 223), (560, y + 223)])
    chip(c, 700, y + 276, "trusted by hephaestusd (1)")
    c.text(48, y + 294, "Child permissions: equal or narrower runs; wider is refused.", size=13.5, fill=BRIGHT)

    # 3. Running an agent
    y = 680
    band(c, y, 316, "3", "Running an agent", "sandbox")
    node(c, 48, y + 58, 200, 90, "Run spec", "Genome, task, commit, and budget, pinned first")
    node(c, 296, y + 58, 220, 90, "Private worktree", "the pinned Git commit, inside a sandbox")
    c.box(564, y + 58, 348, 90, radius=10)
    c.text(578, y + 83, "One runner executes it", size=15, weight=700, fill=INK)
    x = 578
    for i, label in enumerate(("offline reference", "Codex", "Claude")):
        chip(c, x, y + 104, label, color=EMBER if i == 0 else DIM)
        x += len(label) * 7.2 + 20 + 10
    c.arrow([(248, y + 103), (292, y + 103)])
    c.arrow([(516, y + 103), (560, y + 103)])
    node(c, 48, y + 184, 270, 78, "Evidence-required wrapper", "a run that can't record evidence is stopped")
    node(c, 366, y + 184, 230, 78, "Redacted traces", "what happened, secrets scrubbed")
    node(c, 644, y + 184, 268, 78, "Signed run result", "signed with the daemon's own key")
    c.arrow([(700, y + 148), (700, y + 166), (183, y + 166), (183, y + 180)])
    c.text(200, y + 161, "watches every run", size=12.5)
    c.arrow([(830, y + 148), (830, y + 180)])
    c.arrow([(318, y + 223), (362, y + 223)])
    chip(c, 366, y + 276, "written to the record (5)")
    chip(c, 644, y + 276, "to the Arena (4) and record (5)")

    # 4. Measuring in the Arena
    y = 1014
    band(c, y, 400, "4", "Measuring in the Arena", "arena")
    node(c, 48, y + 58, 210, 90, "Paired scheduler", "same tasks, seed, and budget for both")
    node(c, 306, y + 58, 180, 40, "Parent process")
    node(c, 306, y + 108, 180, 40, "Candidate process")
    node(c, 534, y + 58, 190, 200, "Arena", "checks every signed result, then asks the judge to score them", stroke=GOLD, width=2)
    node(c, 772, y + 58, 140, 200, "The judge", "an isolated evaluator, hashed by the World; agents never see it; returns only totals")
    c.arrow([(258, y + 78), (302, y + 78)])
    c.arrow([(258, y + 128), (302, y + 128)])
    c.arrow([(486, y + 78), (530, y + 78)])
    c.arrow([(486, y + 128), (530, y + 128)])
    c.arrow([(724, y + 140), (768, y + 140)])
    c.arrow([(768, y + 176), (728, y + 176)])
    c.text(748, y + 128, "score", size=12, anchor="middle")
    c.text(748, y + 196, "totals", size=12, anchor="middle")
    node(c, 48, y + 172, 220, 78, "Visible and sealed tasks", "from the World; your view hides the answers")
    node(c, 296, y + 172, 200, 78, "Producer's public key", "anchored in the World")
    c.arrow([(496, y + 211), (530, y + 211)])
    c.arrow([(158, y + 250), (158, y + 264), (515, y + 264), (515, y + 240), (530, y + 240)])
    node(c, 48, y + 290, 250, 78, "Verified evaluation event", "written once, idempotent on retry")
    node(c, 346, y + 290, 250, 78, "Candidate-safe summary", "visible results only")
    node(c, 644, y + 290, 268, 78, "Operator-only evidence", "feeds deterministic selection")
    c.arrow([(629, y + 258), (629, y + 276), (173, y + 276), (173, y + 286)])
    c.arrow([(298, y + 329), (342, y + 329)])
    c.arrow([(298, y + 352), (320, y + 352), (320, y + 384), (778, y + 384), (778, y + 372)])

    # 5. The record
    y = 1432
    band(c, y, 196, "5", "The record", "ledger")
    node(c, 48, y + 58, 180, 90, "Declared lifecycle", "only allowed state changes")
    node(c, 272, y + 58, 190, 90, "Hash-linked event", "chained to the one before")
    node(c, 506, y + 58, 170, 40, "SQLite (WAL)")
    node(c, 506, y + 108, 170, 40, "BLAKE3 file store")
    node(c, 720, y + 58, 192, 90, "Verified replay", "must match live state, or the daemon won't start", stroke=GOLD, width=2)
    c.arrow([(228, y + 103), (268, y + 103)])
    c.arrow([(462, y + 78), (502, y + 78)])
    c.arrow([(462, y + 128), (502, y + 128)])
    c.arrow([(676, y + 78), (716, y + 90)])
    c.arrow([(676, y + 128), (716, y + 116)])
    chip(c, 720, y + 158, "rebuilds hephaestusd (1)")
    return c.svg()


DIAGRAMS = {
    "how-it-works.svg": how_it_works,
    "glossary.svg": glossary_readme,
    "glossary-full.svg": glossary_full,
    "under-the-hood.svg": under_the_hood,
    "senate.svg": senate,
    "tour.svg": tour,
    "cli-map.svg": cli_map,
    "world-anatomy.svg": world_anatomy,
    "genome-anatomy.svg": genome_anatomy,
    "run-sandbox.svg": run_sandbox,
    "trace-pipeline.svg": trace_pipeline,
    "evolution-loop.svg": evolution_loop,
    "canary-rollout.svg": canary_rollout,
    "gene-flow.svg": gene_flow,
    "meta-evolution.svg": meta_evolution,
    "mcp-gateway.svg": mcp_gateway,
    "remote-workers.svg": remote_workers,
    "threat-model.svg": threat_model,
    "constitution.svg": constitution,
    "crate-map.svg": crate_map,
    "system-map.svg": system_map,
}


def main(names: list[str]) -> None:
    for name, build in DIAGRAMS.items():
        if names and name not in names:
            continue
        (ASSETS / name).write_text(build(), encoding="utf-8")
        print(f"wrote docs/assets/{name}")


if __name__ == "__main__":
    main(sys.argv[1:])
