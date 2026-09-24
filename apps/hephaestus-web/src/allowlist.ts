import type {Command} from '../../hephaestus-tui/src/protocol.js';

/**
 * The web console never forwards a mutating command to the daemon. This is
 * the single source of truth for that boundary: every command tag the
 * daemon understands is enumerated in {@link MUTATING_COMMANDS} below so a
 * newly added daemon command defaults to refused, not silently allowed.
 */
export const READ_ONLY_COMMANDS = [
	'status',
	'world_list',
	'genome_list',
	'genome_show',
	'genome_prompt',
	'champion_show',
	'job_status',
] as const;

export type ReadOnlyCommandTag = (typeof READ_ONLY_COMMANDS)[number];

const READ_ONLY_SET: ReadonlySet<string> = new Set(READ_ONLY_COMMANDS);

/**
 * Every other command `apps/hephaestus-tui/src/protocol.ts` currently
 * defines, all of which mutate canonical daemon state. Never used to grant
 * access (the allowlist above does that); kept only so a reviewer can see
 * at a glance that this list and the allowlist partition the TUI client's
 * full `Command` union with no overlap and no gaps.
 */
export const NOT_ALLOWED_COMMANDS = [
	'freeze',
	'unfreeze',
	'kill_all',
	'job_kill',
	'champion_rollback',
] as const;

export type CommandRequestBody = {command: unknown; genome_id?: unknown; job_id?: unknown; world_id?: unknown};

const ID_PATTERN = /^[\w.:-]{1,256}$/;

function isId(value: unknown): value is string {
	return typeof value === 'string' && ID_PATTERN.test(value);
}

/**
 * Validate an untrusted JSON request body against the read-only allowlist
 * and return a strictly typed daemon {@link Command}, or `undefined` if the
 * body names a command outside the allowlist or is missing/malformed
 * required identifiers. This is the only place an HTTP request body is
 * turned into a daemon command; there is no fallback path.
 */
export function toAllowedCommand(body: unknown): Command | undefined {
	if (typeof body !== 'object' || body === null || Array.isArray(body)) return undefined;
	const record = body as CommandRequestBody;
	const tag = record.command;
	if (typeof tag !== 'string' || !READ_ONLY_SET.has(tag)) return undefined;
	const commandTag = tag as ReadOnlyCommandTag;
	switch (commandTag) {
		case 'status':
		case 'world_list':
		case 'genome_list':
			return {command: commandTag};
		case 'genome_show':
		case 'genome_prompt':
			return isId(record.genome_id) ? {command: commandTag, genome_id: record.genome_id} : undefined;
		case 'champion_show':
			return isId(record.world_id) ? {command: commandTag, world_id: record.world_id} : undefined;
		case 'job_status':
			return isId(record.job_id) ? {command: commandTag, job_id: record.job_id} : undefined;
	}
}
