import {createConnection, type Socket} from 'node:net';
import {constants as fsConstants, promises as fs, type Stats} from 'node:fs';
import {randomUUID} from 'node:crypto';
import {homedir} from 'node:os';
import {join, resolve} from 'node:path';
import {MAX_FRAME_BYTES, SOCKET_TIMEOUT_MS, parseResponse, type ApiRequest, type ApiResponse, type Command} from './protocol.js';

export type ClientOptions = {dataDir?: string; timeoutMs?: number; maxFrameBytes?: number};

export class ControlClient {
	readonly dataDir: string;
	private readonly timeoutMs: number;
	private readonly maxFrameBytes: number;
	constructor(options: ClientOptions = {}) {
		this.dataDir = resolve(options.dataDir ?? process.env['HEPHAESTUS_HOME'] ?? join(homedir(), '.hephaestus'));
		this.timeoutMs = options.timeoutMs ?? SOCKET_TIMEOUT_MS;
		this.maxFrameBytes = options.maxFrameBytes ?? MAX_FRAME_BYTES;
	}

	async request(command: Command): Promise<ApiResponse> {
		let dir: Stats;
		try {
			dir = await fs.lstat(this.dataDir);
		} catch {
			throw new Error('daemon data directory is unavailable');
		}
		if (!dir.isDirectory() || dir.uid !== process.getuid?.() || (dir.mode & 0o077) !== 0) throw new Error('data directory must be an owner-only directory (0700)');
		const tokenPath = join(this.dataDir, 'operator.token');
	let tokenFile: Awaited<ReturnType<typeof fs.open>>;
	try {
		tokenFile = await fs.open(tokenPath, fsConstants.O_RDONLY | fsConstants.O_NOFOLLOW | fsConstants.O_NONBLOCK);
	} catch (error) {
		if (error && typeof error === 'object' && 'code' in error && error.code === 'ELOOP') {
			throw new Error('operator token must be an owner-only file (0600)');
		}
		throw new Error('operator token is unavailable');
	}
	let token: string;
	try {
		const tokenStat = await tokenFile.stat();
		if (!tokenStat.isFile() || tokenStat.uid !== process.getuid?.() || (tokenStat.mode & 0o077) !== 0) throw new Error('operator token must be an owner-only file (0600)');
		if (tokenStat.size !== 64) throw new Error('operator token file is malformed');
		const bytes = Buffer.alloc(65);
		let read = 0;
		while (read < bytes.length) {
			const result = await tokenFile.read(bytes, read, bytes.length - read, read);
			if (result.bytesRead === 0) break;
			read += result.bytesRead;
		}
		if (read !== 64) throw new Error('operator token file is malformed');
		token = bytes.subarray(0, read).toString('utf8');
	} finally {
		await tokenFile.close();
	}
	if (!/^[a-f0-9]{64}$/.test(token)) throw new Error('operator token file is malformed');
		const request: ApiRequest = {version: 1, request_id: `tui-${process.pid}-${randomUUID()}`, token, command};
		const payload = Buffer.from(JSON.stringify(request));
		if (payload.length > this.maxFrameBytes) throw new Error('request exceeds protocol limit');
		const socketPath = join(this.dataDir, 'control.sock');
		let socketStat: Stats;
		try {
			socketStat = await fs.lstat(socketPath);
		} catch {
			throw new Error('daemon socket is unavailable');
		}
		if (!socketStat.isSocket() || socketStat.uid !== process.getuid?.() || (socketStat.mode & 0o077) !== 0) throw new Error('control socket is not an owner-only socket');
		return new Promise((resolvePromise, reject) => {
			let settled = false;
			let received = 0;
			const chunks: Buffer[] = [];
			const socket: Socket = createConnection(socketPath);
			const deadline = setTimeout(() => finish(new Error('daemon request timed out')), this.timeoutMs);
			const finish = (error?: Error, response?: ApiResponse) => {
				if (settled) return;
				settled = true;
				clearTimeout(deadline);
				socket.destroy();
				if (error) reject(error);
				else resolvePromise(response!);
			};
			socket.once('connect', () => socket.end(payload));
			socket.on('data', chunk => {
				received += chunk.length;
				if (received > this.maxFrameBytes) return finish(new Error('daemon response exceeds protocol limit'));
				chunks.push(chunk);
			});
			socket.once('error', () => finish(new Error('daemon socket request failed')));
			socket.once('end', () => {
				try {
					const responseText = Buffer.concat(chunks).toString('utf8').split(token).join('[REDACTED]');
					finish(undefined, parseResponse(responseText, request.request_id));
				} catch (error) {
					finish(error instanceof Error ? error : new Error('daemon response is invalid'));
				}
			});
		});
	}
}
