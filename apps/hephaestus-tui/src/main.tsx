import React from 'react';
import {render} from 'ink';
import {App} from './ui.js';

const {waitUntilExit} = render(<App />, {exitOnCtrlC: true});
await waitUntilExit();
