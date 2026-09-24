import assert from 'node:assert/strict';
import {mkdtemp, chmod, writeFile, symlink, rm} from 'node:fs/promises';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import test from 'node:test';
import {ControlClient} from '../src/client.js';

test('rejects oversized and non-regular operator tokens before socket access', async () => {
	const dataDir = await mkdtemp(join(tmpdir(), 'hephaestus-tui-token-'));
	const tokenPath = join(dataDir, 'operator.token');
	try {
		await chmod(dataDir, 0o700);
		await writeFile(tokenPath, 'a'.repeat(1024), {mode: 0o600});
		const client = new ControlClient({dataDir});
		await assert.rejects(client.request({command: 'status'}), /operator token file is malformed/);
		await rm(tokenPath);
		await symlink(join(dataDir, 'missing'), tokenPath);
		await assert.rejects(client.request({command: 'status'}), /operator token must be an owner-only file/);
	} finally {
		await rm(dataDir, {recursive: true, force: true});
	}
});
