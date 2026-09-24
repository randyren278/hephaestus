import {randomInt} from 'node:crypto';
import {fileURLToPath} from 'node:url';
import {dirname, join} from 'node:path';
import {ControlClient} from '../../hephaestus-tui/src/client.js';
import {createWebServer} from './server.js';
import {generateSessionToken} from './security.js';

const here = dirname(fileURLToPath(import.meta.url));
// `src/main.ts` ships its static console from `src/web/`; the bundled
// `dist/main.mjs` produced by `npm run build` ships it from `public/`
// next to itself. Prefer whichever exists next to this file.
const staticRoot = join(here, 'web');

const client = new ControlClient({dataDir: process.env['HEPHAESTUS_HOME']});
const sessionToken = generateSessionToken();

const requestedPort = process.env['HEPHAESTUS_WEB_PORT'] ? Number(process.env['HEPHAESTUS_WEB_PORT']) : undefined;

async function listenOnPort(port: number): Promise<number> {
	const server = createWebServer({
		requestDaemon: command => client.request(command),
		sessionToken,
		port,
		staticRoot,
	});
	return new Promise((resolve, reject) => {
		server.once('error', reject);
		server.listen(port, '127.0.0.1', () => resolve(port));
	});
}

async function start(): Promise<void> {
	if (requestedPort !== undefined) {
		await listenOnPort(requestedPort);
		announce(requestedPort);
		return;
	}
	for (let attempt = 0; attempt < 8; attempt += 1) {
		const candidate = randomInt(20_000, 65_000);
		try {
			await listenOnPort(candidate);
			announce(candidate);
			return;
		} catch (error) {
			if (!(error instanceof Error) || !('code' in error) || error.code !== 'EADDRINUSE') throw error;
		}
	}
	throw new Error('could not find a free local port after 8 attempts');
}

function announce(port: number): void {
	// The token travels as a URL fragment, which browsers never send to the
	// server (unlike a query string), and the page reads it client-side to
	// attach as a header on every API call.
	console.log(`Hephaestus web console: http://127.0.0.1:${port}/#token=${sessionToken}`);
	console.log('Bound to 127.0.0.1 only. Read-only. Ctrl+C to stop.');
}

await start();
