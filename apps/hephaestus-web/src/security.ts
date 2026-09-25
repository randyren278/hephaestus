import {randomBytes, timingSafeEqual} from 'node:crypto';

/** Generate a fresh 256-bit per-launch session token, hex encoded. */
export function generateSessionToken(): string {
	return randomBytes(32).toString('hex');
}

/** Constant-time comparison so an invalid guess cannot be timed. */
export function tokensMatch(expected: string, candidate: unknown): boolean {
	if (typeof candidate !== 'string' || candidate.length !== expected.length) return false;
	return timingSafeEqual(Buffer.from(expected, 'utf8'), Buffer.from(candidate, 'utf8'));
}

const ALLOWED_HOSTNAMES = new Set(['127.0.0.1', 'localhost', '[::1]', '::1']);

/**
 * Defends against DNS rebinding: the browser's `Host` header must name this
 * loopback server on the exact port it is bound to, no matter what hostname
 * a page in the browser's address bar used to get there. A request whose
 * `Host` fails this check is refused before anything else runs.
 */
export function hostIsAllowed(hostHeader: string | undefined, port: number): boolean {
	if (!hostHeader) return false;
	const at = hostHeader.lastIndexOf(':');
	const hostname = at === -1 ? hostHeader : hostHeader.slice(0, at);
	const portPart = at === -1 ? undefined : hostHeader.slice(at + 1);
	if (!ALLOWED_HOSTNAMES.has(hostname.toLowerCase())) return false;
	if (portPart === undefined) return false;
	return Number(portPart) === port;
}

/**
 * Defends against cross-site request forgery: when a browser sends an
 * `Origin` header (every fetch/XHR does), it must match this loopback
 * server exactly. Requests with no `Origin` header (same-origin navigation,
 * curl) are allowed through this check alone; the session token check
 * still applies to every API call.
 */
export function originIsAllowed(originHeader: string | undefined, port: number): boolean {
	if (originHeader === undefined) return true;
	try {
		const origin = new URL(originHeader);
		return hostIsAllowed(origin.port ? `${origin.hostname}:${origin.port}` : origin.hostname, port) && origin.protocol === 'http:';
	} catch {
		return false;
	}
}

/** Strict CSP: no inline script/style, no remote fetches, nothing framed in, no CORS. */
export const CONTENT_SECURITY_POLICY = [
	"default-src 'self'",
	"script-src 'self'",
	"style-src 'self'",
	"img-src 'self'",
	"connect-src 'self'",
	"frame-ancestors 'none'",
	"base-uri 'none'",
	"form-action 'none'",
].join('; ');

export const SECURITY_HEADERS: Record<string, string> = {
	'Content-Security-Policy': CONTENT_SECURITY_POLICY,
	'X-Content-Type-Options': 'nosniff',
	'X-Frame-Options': 'DENY',
	'Referrer-Policy': 'no-referrer',
	'Cross-Origin-Resource-Policy': 'same-origin',
	'Cross-Origin-Opener-Policy': 'same-origin',
};
