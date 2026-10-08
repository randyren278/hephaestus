import type {ApiResponse, Command} from '../../../hephaestus-tui/src/protocol.js';

const SESSION_KEY = 'hephaestus.web.session';

/** Keep reloads in this tab working without leaving credentials in the URL or localStorage. */
export function sessionToken(hash: string, storage: Pick<Storage, 'getItem' | 'setItem' | 'removeItem'>): string {
	const supplied = new URLSearchParams(hash.replace(/^#/, '')).get('token');
	try {
		if (supplied !== null) {
			if (!/^[a-f0-9]{64}$/.test(supplied)) { storage.removeItem(SESSION_KEY); return ''; }
			storage.setItem(SESSION_KEY, supplied);
			return supplied;
		}
		const saved = storage.getItem(SESSION_KEY) ?? '';
		return /^[a-f0-9]{64}$/.test(saved) ? saved : '';
	} catch {
		return supplied !== null && /^[a-f0-9]{64}$/.test(supplied) ? supplied : '';
	}
}

export function clearSession(storage: Pick<Storage, 'removeItem'>): void {
	try { storage.removeItem(SESSION_KEY); } catch { /* Private browsing may disable storage. */ }
}

/** Convert connection, timeout and non-JSON failures into the same visible error path as daemon errors. */
export async function requestCommand(command: Command, token: string, send: typeof fetch = fetch): Promise<ApiResponse> {
	const failure = (code: string, message: string): ApiResponse => ({version: 1, request_id: 'web', error: {code, message}});
	if (!token) return failure('unauthorized', 'Open the full URL printed by the running web console to connect this tab.');
	try {
		const response = await send('/api/command', {
			method: 'POST',
			headers: {'Content-Type': 'application/json', 'x-hephaestus-web-token': token},
			body: JSON.stringify(command),
			signal: AbortSignal.timeout(25_000),
		});
		if (response.status === 401) return failure('session_expired', 'This console session has expired. Open the new URL printed by the running web console.');
		const payload = await response.json() as ApiResponse;
		if (payload.error && typeof payload.error.code === 'string' && typeof payload.error.message === 'string') return payload;
		if (!response.ok || payload.version !== 1 || typeof payload.request_id !== 'string' || !payload.data) return failure('connection', 'The console returned an unexpected response. Retry with Refresh.');
		return payload;
	} catch {
		return failure('connection', 'Cannot reach the local console. Check that it is running, then retry with Refresh.');
	}
}
