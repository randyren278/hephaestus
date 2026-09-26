import React from 'react';
import {render} from 'ink';
import {App} from './ui.js';

// The tour plays only when asked: `heph` passes `--tour` the first time a
// data directory has never completed one (see `tour.ts`'s marker file), and
// `heph --tour` or `hephaestus tui --tour` replays it. A plain `hephaestus tui`
// always opens the home menu, so scripts and PTY tests see a stable screen.
const forceTour = process.argv.includes('--tour') || process.argv.includes('--first-run');

// The operator console is always interactive on a real terminal, even when a
// `CI` variable is exported; Ink would otherwise draw only the final frame.
const {waitUntilExit} = render(<App forceTour={forceTour} />, {exitOnCtrlC: true, interactive: Boolean(process.stdout.isTTY)});
await waitUntilExit();
