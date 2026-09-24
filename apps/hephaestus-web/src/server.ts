import {createServer as createHttpServer, type IncomingMessage, type ServerResponse, type Server} from 'node:http';
import {readFile} from 'node:fs/promises';
import {join} from 'node:path';
import type {ApiResponse, Command} from '../../hephaestus-tui/src/protocol.js';
import {toAllowedCommand} from './allowlist.js';
import {SECURITY_HEADERS, hostIsAllowed, originIsAllowed, tokensMatch} from './security.js';

export type WebServerOptions = {
	/** Send one typed command to the daemon and resolve its typed response, or reject. Never called with a command outside the read-only allowlist. */
	requestDaemon: (command: Command) => Promise<ApiResponse>;
	/** Per-launch random session token every API call must present. */
	sessionToken: string;
	/** The exact port this server is bound to, used to validate `Host` and `Origin`. */
	port: number;
	/** Directory containing the bundled static console (index.html, app.js, styles.css). */
	staticRoot: string;
	/** Maximum accepted request body size in bytes, guarding the API endpoint. */
	maxBodyBytes?: number;
};

const STATIC_FILES: Record<string, {file: string; contentType: string}> = {
	'/': {file: 'index.html', contentType: 'text/html; charset=utf-8'},
	'/index.html': {file: 'index.html', contentType: 'text/html; charset=utf-8'},
	'/app.js': {file: 'app.js', contentType: 'text/javascript; charset=utf-8'},
	'/styles.css': {file: 'styles.css', contentType: 'text/css; charset=utf-8'},
};

const TOKEN_HEADER = 'x-hephaestus-web-token';

function sendJson(res: ServerResponse, status: number, body: unknown): void {
	const payload = Buffer.from(JSON.stringify(body));
	res.writeHead(status, {'Content-Type': 'application/json; charset=utf-8', 'Content-Length': String(payload.length)});
	res.end(payload);
}

function readBody(req: IncomingMessage, maxBytes: number): Promise<Buffer> {
	return new Promise((resolve, reject) => {
		const chunks: Buffer[] = [];
		let received = 0;
		req.on('data', (chunk: Buffer) => {
			received += chunk.length;
			if (received > maxBytes) {
				reject(new Error('request body exceeds limit'));
				req.destroy();
				return;
			}
			chunks.push(chunk);
		});
		req.on('end', () => resolve(Buffer.concat(chunks)));
		req.on('error', reject);
	});
}

/**
 * Build the local read-only web console server. Every request, static or
 * API, passes the same `Host` allowlist first; every API request also needs
 * a matching `Origin` (when the browser sends one) and the exact per-launch
 * session token. No handler here ever accepts a command outside
 * {@link toAllowedCommand}'s allowlist, and no response sets a CORS header.
 */
export function createWebServer(options: WebServerOptions): Server {
	const maxBodyBytes = options.maxBodyBytes ?? 8192;

	return createHttpServer((req, res) => {
		for (const [name, value] of Object.entries(SECURITY_HEADERS)) res.setHeader(name, value);

		if (!hostIsAllowed(req.headers.host, options.port)) {
			sendJson(res, 421, {error: {code: 'invalid_request', message: 'unrecognized Host header'}});
			return;
		}

		const url = new URL(req.url ?? '/', `http://${req.headers.host}`);

		if (req.method === 'GET' && STATIC_FILES[url.pathname]) {
			const entry = STATIC_FILES[url.pathname]!;
			readFile(join(options.staticRoot, entry.file))
				.then(contents => {
					res.writeHead(200, {'Content-Type': entry.contentType, 'Content-Length': String(contents.length)});
					res.end(contents);
				})
				.catch(() => sendJson(res, 404, {error: {code: 'not_found', message: 'asset unavailable'}}));
			return;
		}

		if (url.pathname === '/api/command') {
			if (req.method !== 'POST') {
				sendJson(res, 405, {error: {code: 'invalid_request', message: 'method not allowed'}});
				return;
			}
			if (!originIsAllowed(req.headers.origin, options.port)) {
				sendJson(res, 403, {error: {code: 'unauthorized', message: 'unrecognized Origin header'}});
				return;
			}
			if (!tokensMatch(options.sessionToken, req.headers[TOKEN_HEADER])) {
				sendJson(res, 401, {error: {code: 'unauthorized', message: 'missing or invalid session token'}});
				return;
			}
			readBody(req, maxBodyBytes)
				.then(async raw => {
					let parsed: unknown;
					try {
						parsed = raw.length === 0 ? {} : JSON.parse(raw.toString('utf8'));
					} catch {
						sendJson(res, 400, {error: {code: 'invalid_request', message: 'malformed JSON body'}});
						return;
					}
					const command = toAllowedCommand(parsed);
					if (!command) {
						sendJson(res, 400, {error: {code: 'invalid_request', message: 'command is not on the read-only allowlist'}});
						return;
					}
					try {
						const response = await options.requestDaemon(command);
						sendJson(res, 200, response);
					} catch (error) {
						sendJson(res, 502, {error: {code: 'internal', message: error instanceof Error ? error.message : 'daemon request failed'}});
					}
				})
				.catch(() => sendJson(res, 413, {error: {code: 'invalid_request', message: 'request body exceeds limit'}}));
			return;
		}

		sendJson(res, 404, {error: {code: 'not_found', message: 'unknown path'}});
	});
}
