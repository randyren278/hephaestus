/**
 * The hero's palette, ported into named tokens so every screen in the
 * operator console draws from one source of truth instead of ad hoc Ink
 * color names. The seven hex values below are copied verbatim from
 * `scripts/pixel_art/generate.py` (`PALETTE`), which is itself the palette
 * used for the README hero image and the web console — keeping the TUI in
 * the same visual family as the rest of the project.
 *
 * Terminals vary in what they can show, so every color is resolved through
 * a `ColorLevel`:
 *   - `truecolor`: the hex values render as-is.
 *   - `basic`: a curated 16-color approximation (see `FALLBACK_16`) is used
 *     instead, for terminals that only understand the ANSI 16-color set.
 *   - `none`: no color codes are emitted at all — `NO_COLOR` is set, the
 *     process is not attached to a TTY, or the terminal identifies as
 *     having no color support (`TERM=dumb` or unset).
 */
import {useMemo} from 'react';

export const PALETTE = {
	bg: '#10131a',
	bgRaised: '#181c26',
	bgCard: '#1e2330',
	ink: '#e9e4d8',
	inkDim: '#a2a7b5',
	ember: '#e8590c',
	emberBright: '#ff8a3d',
	gold: '#f4c542',
	gray: '#5b6270',
	danger: '#c0392b',
	border: '#3a3f4d',
} as const;

export type PaletteToken = keyof typeof PALETTE;

/** A conservative 16-color (ANSI) stand-in for each hex token, for terminals without truecolor. */
const FALLBACK_16: Record<PaletteToken, string> = {
	bg: 'black',
	bgRaised: 'black',
	bgCard: 'black',
	ink: 'white',
	inkDim: 'gray',
	ember: 'red',
	emberBright: 'yellowBright',
	gold: 'yellow',
	gray: 'gray',
	danger: 'redBright',
	border: 'gray',
};

export type ColorLevel = 'none' | 'basic' | 'truecolor';

/**
 * Semantic roles used by the screens. Every screen colors things by role,
 * never by raw palette token or Ink color name, so the hero's palette stays
 * the single source of truth and a future re-palette only touches this file.
 */
export type Role =
	| 'champion' // the Champion / candidate lane — the hero's orange crest
	| 'championBright' // the champion's brighter accent (flames, highlights)
	| 'challenger' // the parent / standby lane — the hero's graphite crest
	| 'challengerDim' // dimmer challenger text
	| 'judge' // the hooded judge and sealed evaluator — gold
	| 'sealed' // alias of judge, used where "sealed receipt" reads better
	| 'danger' // regressions, aborts, rollbacks
	| 'regression' // alias of danger, used in evidence/cost screens
	| 'success' // promotions, positive outcomes
	| 'improvement' // alias of success, used in evidence/cost screens
	| 'ink' // primary foreground text
	| 'inkDim' // secondary / de-emphasized text
	| 'border' // panel borders and rules
	| 'muted'; // neutral, unselected, "nothing to report" text

const ROLE_TOKEN: Record<Role, PaletteToken> = {
	champion: 'ember',
	championBright: 'emberBright',
	challenger: 'gray',
	challengerDim: 'inkDim',
	judge: 'gold',
	sealed: 'gold',
	danger: 'danger',
	regression: 'danger',
	success: 'emberBright',
	improvement: 'ember',
	ink: 'ink',
	inkDim: 'inkDim',
	border: 'border',
	muted: 'gray',
};

/** Block-glyph pixel grid for the hero's torch-lit colosseum banner (64x14). Ported verbatim from `scripts/pixel_art/generate.py`'s `_banner()`/`BANNER`, not regenerated at runtime — re-run that script and re-paste here if the grid changes. */
export const TUI_BANNER: readonly string[] = [
	'bbbbbbbbbbEbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbdbbbbbbbbbbb',
	'bbbbbbbbbEbEbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbdbdbbbbbbbbbb',
	'bbbbbbbbbbobbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbobbbbbbbbbbb',
	'bbbbbbbbbbobbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbobbbbbbbbbbb',
	'bbbbbbbbbbobbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbobbbbbbbbbbb',
	'bbbbbbbbbbobbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbobbbbbbbbbbb',
	'bbbbyyyyybobyyyyybbbyyyyybbbyyyyybbbyyyyybbbyyyyybbboyyyybbbbbbb',
	'bbbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbbbbb',
	'bbbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbbbbb',
	'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
	'bbbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbbbbb',
	'bbbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbybbbbbbb',
	'oooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooooo',
	'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
];

/** Maps a `TUI_BANNER` character to the palette token it stands for (mirrors `generate.py`'s `PALETTE` keys); `null` means transparent/background — the home screen skips it rather than drawing a solid block. */
export const BANNER_CHAR_TOKEN: Record<string, PaletteToken | null> = {
	'.': null,
	b: null, // background pixels read as empty space in a terminal, not a solid block
	r: 'bgRaised',
	c: 'bgCard',
	i: 'ink',
	d: 'inkDim',
	e: 'ember',
	E: 'emberBright',
	g: 'gold',
	y: 'gray',
	n: 'danger',
	o: 'border',
};

/** Rows picked out of the 14-row `TUI_BANNER` grid that read as a recognizable torch-and-arch silhouette in the four lines the home screen budgets for it (flame tips, flame base, arches-with-torch-poles, ground line). */
export const HOME_BANNER_ROWS: readonly number[] = [0, 1, 6, 12];

/** How a raw palette token reads as a semantic role when a pixel-art sprite (like `TUI_BANNER`) is rendered as block glyphs. */
export const TOKEN_ROLE: Record<PaletteToken, Role> = {
	bg: 'muted', bgRaised: 'muted', bgCard: 'muted',
	ink: 'ink', inkDim: 'challengerDim',
	ember: 'champion', emberBright: 'championBright',
	gold: 'judge', gray: 'challenger', danger: 'danger', border: 'border',
};

export const GLYPHS = {
	caret: '›',
	unselectedCaret: ' ',
	championCrest: '▲',
	challengerCrest: '◆',
	quarantined: '✕',
	seal: '◈',
	helixA: '⟋',
	helixB: '⟍',
	ruleHeavy: '━',
	ruleLight: '─',
	spark: '✦',
	sparkDim: '✧',
} as const;

/**
 * Resolves how much color this process's terminal should get, from the
 * environment and TTY state alone (no React needed — this is what
 * `test/theme.test.tsx` exercises directly). `env`/`isTTY` are injectable so
 * tests don't have to mutate real process state.
 */
export function resolveColorLevel(
	env: NodeJS.ProcessEnv = process.env,
	isTTY: boolean = Boolean(process.stdout && process.stdout.isTTY),
): ColorLevel {
	const forceColor = env.FORCE_COLOR;
	if (forceColor === '0') return 'none';
	// NO_COLOR (https://no-color.org): any non-empty value disables color,
	// unless the operator explicitly overrides it with FORCE_COLOR.
	if (env.NO_COLOR !== undefined && env.NO_COLOR !== '' && forceColor === undefined) return 'none';
	if (forceColor !== undefined && forceColor !== '') {
		if (forceColor === '3' || forceColor.toLowerCase() === 'true') return 'truecolor';
		return 'basic';
	}
	if (!isTTY) return 'none';
	const colorterm = (env.COLORTERM ?? '').toLowerCase();
	if (colorterm === 'truecolor' || colorterm === '24bit') return 'truecolor';
	const term = (env.TERM ?? '').toLowerCase();
	if (!term || term === 'dumb') return 'none';
	if (term.includes('direct')) return 'truecolor';
	return 'basic';
}

/** Resolves a role to the Ink `color` prop value for a given level, or `undefined` for "no color" (plain text). */
export function colorFor(role: Role, level: ColorLevel): string | undefined {
	if (level === 'none') return undefined;
	const token = ROLE_TOKEN[role];
	return level === 'truecolor' ? PALETTE[token] : FALLBACK_16[token];
}

export type Theme = {
	level: ColorLevel;
	/** `false` exactly when `level === 'none'` — plain text, no escape codes. */
	enabled: boolean;
	color(role: Role): string | undefined;
	palette: typeof PALETTE;
	glyphs: typeof GLYPHS;
};

export function buildTheme(level: ColorLevel): Theme {
	return {
		level,
		enabled: level !== 'none',
		color: role => colorFor(role, level),
		palette: PALETTE,
		glyphs: GLYPHS,
	};
}

/**
 * Ink's `<Text>` props are plain-optional (`color?: string`), which
 * `exactOptionalPropertyTypes` treats as different from
 * `color?: string | undefined`. Every screen resolves colors through
 * `theme.color(role)`, which is `undefined` at `ColorLevel: 'none'` — this
 * builds a props object that only has the keys actually set, so spreading a
 * possibly-`undefined` theme color into `<Text {...colorProps(...)}>` never
 * trips that check.
 */
export function colorProps(color: string | undefined, bold?: boolean): {color?: string; bold?: boolean} {
	const props: {color?: string; bold?: boolean} = {};
	if (color !== undefined) props.color = color;
	if (bold) props.bold = bold;
	return props;
}

/** Same idea as `colorProps`, for Ink's `borderColor` prop on `<Box>`. */
export function borderColorProps(color: string | undefined): {borderColor?: string} {
	return color !== undefined ? {borderColor: color} : {};
}

export type ThemeOptions = {env?: NodeJS.ProcessEnv; isTTY?: boolean};

/** The hook every screen uses to get themed colors and glyphs. `opts` is for tests only — real call sites read the live process. */
export function useTheme(opts?: ThemeOptions): Theme {
	const level = useMemo(
		() => resolveColorLevel(opts?.env, opts?.isTTY),
		// eslint-disable-next-line react-hooks/exhaustive-deps
		[opts?.env, opts?.isTTY],
	);
	return useMemo(() => buildTheme(level), [level]);
}
