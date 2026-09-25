import {existsSync, mkdirSync, writeFileSync} from 'node:fs';
import {homedir} from 'node:os';
import {dirname, isAbsolute, join} from 'node:path';

/** Default workspace directory for Markdown Genome sources authored through the TUI. */
export const DEFAULT_AGENT_WORKSPACE = join(homedir(), '.hephaestus', 'agents');

/** Turns a World or agent name into a filesystem-safe basename fragment. */
export function slugify(name: string): string {
	const slug = name.toLowerCase().replace(/[^a-z0-9]+/g, '-').replace(/^-+|-+$/g, '');
	return slug || 'agent';
}

export function defaultAgentPath(worldName: string, workspace = DEFAULT_AGENT_WORKSPACE): string {
	return join(workspace, `${slugify(worldName)}.md`);
}

/** Expands a leading `~` to the operator's home directory; otherwise returns the trimmed input unchanged. */
export function expandPath(input: string): string {
	const trimmed = input.trim();
	if (trimmed === '~') return homedir();
	if (trimmed.startsWith('~/')) return join(homedir(), trimmed.slice(2));
	return trimmed;
}

export function markdownAgentTemplate(name: string): string {
	return `# ${name}\n\n<!-- Hephaestus Markdown Genome source. Edit the prompt below, then save and exit to continue. -->\n\n## Prompt\n\nYou are ${name}. Describe the agent's task-solving approach here.\n`;
}

/**
 * Ensures a Markdown Genome source file exists at `path`, creating its parent
 * workspace directory and a starter template when it is missing, and returns
 * the resolved absolute path. Refuses a non-absolute path because
 * `genome_register` requires one and a relative path would silently resolve
 * against the daemon's own working directory rather than the operator's.
 */
export function ensureAgentSource(path: string, worldName: string): string {
	const resolved = expandPath(path);
	if (!isAbsolute(resolved)) throw new Error('Markdown agent path must be absolute');
	if (!existsSync(resolved)) {
		mkdirSync(dirname(resolved), {recursive: true});
		writeFileSync(resolved, markdownAgentTemplate(worldName), {mode: 0o600});
	}
	return resolved;
}

/** The editor command to hand the terminal off to: `$VISUAL`, then `$EDITOR`, then `vi`. */
export function editorCommand(): string {
	return process.env['VISUAL'] || process.env['EDITOR'] || 'vi';
}
