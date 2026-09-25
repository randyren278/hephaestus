import React from 'react';
import {render} from 'ink';
import {App} from './ui.js';

// The operator console is always interactive on a real terminal, even when a
// `CI` variable is exported; Ink would otherwise draw only the final frame.
const {waitUntilExit} = render(<App />, {exitOnCtrlC: true, interactive: Boolean(process.stdout.isTTY)});
await waitUntilExit();
