import { mkdir, writeFile } from 'node:fs/promises';
import { spawnSync } from 'node:child_process';

await mkdir('dist', { recursive: true });
const build = spawnSync(
	'node_modules/.bin/esbuild',
	[
		'src/main.tsx',
		'--bundle',
		'--platform=node',
		'--target=node24',
		'--format=esm',
		'--alias:react-devtools-core=./src/packaged-devtools.mjs',
		'--outfile=dist/main.bundle.mjs',
	],
	{ stdio: 'inherit' },
);
if (build.error) throw build.error;
if (build.status !== 0) process.exit(build.status ?? 1);

await writeFile(
	'dist/main.mjs',
	[
		"import { createRequire } from 'node:module';",
		'globalThis.require ??= createRequire(import.meta.url);',
		"await import('./main.bundle.mjs');",
		'',
	].join('\n'),
);
