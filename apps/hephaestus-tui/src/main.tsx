import React from 'react';
import {render} from 'ink';
import {App} from './ui.js';
import {readTourMarker, resolveDataDir} from './tour.js';

// `--tour`/`--first-run` force the product tour to (re)play; otherwise it
// shows itself once, the first time this data directory has never completed
// one (see `tour.ts`'s marker file), and never again unless replayed.
const requestedTour = process.argv.includes('--tour') || process.argv.includes('--first-run');
const forceTour = requestedTour || !readTourMarker(resolveDataDir()).completed;

// The operator console is always interactive on a real terminal, even when a
// `CI` variable is exported; Ink would otherwise draw only the final frame.
const {waitUntilExit} = render(<App forceTour={forceTour} />, {exitOnCtrlC: true, interactive: Boolean(process.stdout.isTTY)});
await waitUntilExit();
