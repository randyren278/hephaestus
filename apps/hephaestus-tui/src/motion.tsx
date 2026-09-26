/**
 * Small, deterministic animation primitives for the operator console.
 *
 * Everything here is driven by one process-wide ticker (`subscribe` below):
 * at most one `setInterval`, at ~10fps (`RAW_TICK_MS`), started lazily when
 * the first animated component mounts and stopped when the last one
 * unmounts. Individual components ask `useFrame` for a slower logical frame
 * (their own `intervalMs`) derived from that shared raw tick, so adding more
 * animated widgets on screen never adds more timers.
 *
 * Every primitive takes `animate` and `frame`:
 *   - `frame` (a number) freezes the primitive at that exact logical frame,
 *     with no timer at all — this is what `test/motion.test.tsx` and the
 *     view tests use for deterministic snapshots.
 *   - `animate={false}` renders the primitive's resting/final state, also
 *     with no timer — the default most view-level components fall back to
 *     when `animate` is left unset and the process is not attached to a
 *     TTY (see `useMotionEnabled`), which is why most existing tests need
 *     no changes at all.
 */
import React, {useEffect, useState} from 'react';
import {Text} from 'ink';
import {colorProps, useTheme, type Role} from './theme.js';

/** The shared ticker's own rate: ~10fps, comfortably under the 80-120ms budget. */
export const RAW_TICK_MS = 100;

type Listener = (frame: number) => void;
const listeners = new Set<Listener>();
let rawFrame = 0;
let timer: ReturnType<typeof setInterval> | undefined;

function ensureTicking(): void {
	if (timer || listeners.size === 0) return;
	timer = setInterval(() => {
		rawFrame += 1;
		for (const listener of listeners) listener(rawFrame);
	}, RAW_TICK_MS);
	// Never keep the process alive just for decorative animation.
	if (typeof (timer as {unref?: () => void}).unref === 'function') (timer as unknown as {unref: () => void}).unref();
}

function stopIfIdle(): void {
	if (listeners.size === 0 && timer) {
		clearInterval(timer);
		timer = undefined;
	}
}

function subscribe(listener: Listener): () => void {
	listeners.add(listener);
	ensureTicking();
	return () => {
		listeners.delete(listener);
		stopIfIdle();
	};
}

/** Test-only: how many components currently hold the shared ticker open. */
export function activeTickerListenerCount(): number {
	return listeners.size;
}

/** Whether the shared ticker currently has a live `setInterval` running. */
export function isTickerRunning(): boolean {
	return timer !== undefined;
}

/** Test-only: the shared ticker's raw tick count, to observe that it is genuinely advancing over real time. */
export function debugRawFrame(): number {
	return rawFrame;
}

export function isAttachedTTY(): boolean {
	return Boolean(process.stdout && process.stdout.isTTY);
}

/** Whether animated components should animate by default, absent an explicit `animate` prop. Motion pauses whenever the process is not attached to a TTY (piped output, tests, CI logs). */
export function useMotionEnabled(): boolean {
	return isAttachedTTY();
}

export type FrameOptions = {
	/** Defaults to `useMotionEnabled()` — real terminals animate, piped/non-TTY output does not. */
	animate?: boolean | undefined;
	/** Freezes at this exact logical frame; overrides `animate` and starts no timer. */
	frame?: number | undefined;
};

export type FrameState = {frame: number; live: boolean};

/**
 * Returns a logical frame number that advances roughly every `intervalMs`,
 * backed by the one shared ticker. `live` is `false` when nothing is
 * actually advancing (frozen at a fixed `frame`, `animate` is `false`, or
 * animation is disabled by default) — callers use it to decide between a
 * mid-animation frame and a resting/final appearance.
 */
export function useFrame(intervalMs: number = RAW_TICK_MS, options: FrameOptions = {}): FrameState {
	const motionDefault = useMotionEnabled();
	const {animate = motionDefault, frame: fixedFrame} = options;
	const shouldTick = animate && fixedFrame === undefined;
	const [raw, setRaw] = useState(0);
	useEffect(() => {
		if (!shouldTick) return undefined;
		return subscribe(setRaw);
	}, [shouldTick]);
	if (fixedFrame !== undefined) return {frame: fixedFrame, live: true};
	if (!shouldTick) return {frame: 0, live: false};
	const period = Math.max(1, Math.round(intervalMs / RAW_TICK_MS));
	return {frame: Math.floor(raw / period), live: true};
}

// --- Torch flicker -------------------------------------------------------

const FLICKER_INTERVAL_MS = 260;
const FLICKER_FRAMES: ReadonlyArray<{glyph: string; role: Role}> = [
	{glyph: '▲', role: 'champion'},
	{glyph: '▲', role: 'championBright'},
	{glyph: '◆', role: 'judge'},
];

/** A small torch flame that cycles glyph + color (ember → ember-bright → gold), echoing the hero's torchlight. */
export function TorchFlicker({animate, frame}: FrameOptions = {}) {
	const theme = useTheme();
	const {frame: f} = useFrame(FLICKER_INTERVAL_MS, {animate, frame});
	const step = FLICKER_FRAMES[f % FLICKER_FRAMES.length]!;
	return <Text {...colorProps(theme.color(step.role))}>{step.glyph}</Text>;
}

// --- Typewriter / marquee --------------------------------------------------

const TYPEWRITER_INTERVAL_MS = 45;

/** Reveals `text` once, left to right, then holds — never loops. Renders the full text immediately when not live (the default off a TTY). */
export function Typewriter({text, intervalMs = TYPEWRITER_INTERVAL_MS, animate, frame, color, bold}: FrameOptions & {
	text: string; intervalMs?: number; color?: string | undefined; bold?: boolean | undefined;
}) {
	const {frame: f, live} = useFrame(intervalMs, {animate, frame});
	const shown = live ? text.slice(0, Math.min(text.length, f + 1)) : text;
	return <Text {...colorProps(color, bold)}>{shown}</Text>;
}

// --- Progress bar -----------------------------------------------------------

const PROGRESS_INTERVAL_MS = 120;

/** A segmented progress bar with a moving highlight sweeping across the filled portion while live. */
export function ProgressBar({ratio, width = 16, animate, frame, color, highlightColor, trackColor}: FrameOptions & {
	ratio: number; width?: number; color?: string | undefined; highlightColor?: string | undefined; trackColor?: string | undefined;
}) {
	const {frame: f, live} = useFrame(PROGRESS_INTERVAL_MS, {animate, frame});
	const clamped = Math.max(0, Math.min(1, ratio));
	const filled = Math.round(clamped * width);
	const highlightAt = live && filled > 0 ? f % filled : -1;
	const chars: React.ReactNode[] = [];
	for (let index = 0; index < width; index += 1) {
		if (index < filled) {
			const isHighlight = index === highlightAt;
			chars.push(<Text key={index} {...colorProps(isHighlight ? (highlightColor ?? color) : color, isHighlight)}>█</Text>);
		} else {
			chars.push(<Text key={index} {...colorProps(trackColor)}>·</Text>);
		}
	}
	return <Text>{chars}</Text>;
}

// --- Spinner ------------------------------------------------------------

const SPINNER_INTERVAL_MS = 90;
const SPINNER_FRAMES = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

export function Spinner({animate, frame, color}: FrameOptions & {color?: string | undefined}) {
	const theme = useTheme();
	const {frame: f} = useFrame(SPINNER_INTERVAL_MS, {animate, frame});
	return <Text {...colorProps(color ?? theme.color('judge'))}>{SPINNER_FRAMES[f % SPINNER_FRAMES.length]}</Text>;
}

// --- Crest clash (idle / waiting) -------------------------------------------

const CLASH_INTERVAL_MS = 220;
// Two crests approach, spark, and retreat — a short idle loop for waiting states.
const CLASH_FRAMES: ReadonlyArray<{left: string; mid: string; right: string; spark: boolean}> = [
	{left: '◆', mid: '        ', right: '▲', spark: false},
	{left: ' ◆', mid: '      ', right: '▲ ', spark: false},
	{left: '  ◆', mid: '    ', right: '▲  ', spark: false},
	{left: '   ◆', mid: '✦', right: '▲   ', spark: true},
	{left: '  ◆', mid: '    ', right: '▲  ', spark: false},
	{left: ' ◆', mid: '      ', right: '▲ ', spark: false},
];

/** Two crest glyphs approaching, sparking, and retreating — the idle animation for "waiting on the daemon" states. */
export function CrestClash({animate, frame}: FrameOptions = {}) {
	const theme = useTheme();
	const {frame: f} = useFrame(CLASH_INTERVAL_MS, {animate, frame});
	const step = CLASH_FRAMES[f % CLASH_FRAMES.length]!;
	return <Text>
		<Text {...colorProps(theme.color('challenger'))}>{step.left}</Text>
		<Text {...colorProps(step.spark ? theme.color('judge') : undefined)}>{step.mid}</Text>
		<Text {...colorProps(theme.color('champion'))}>{step.right}</Text>
	</Text>;
}

// --- Celebration burst ----------------------------------------------------

const CELEBRATION_INTERVAL_MS = 120;
const CELEBRATION_FRAME_COUNT = 10; // ~1.2s at 120ms/frame
const CELEBRATION_GLYPHS = ['✦', '✧', '✦', '✧'];

/** A ~1.2s spark burst in gold/ember, shown once while `active` — for a promotion or an eligible selection. Stays visible without a timer when not live (non-TTY), and disappears on its own once the burst plays out while live. */
export function CelebrationBurst({active, label = 'PROMOTION', animate, frame}: FrameOptions & {active: boolean; label?: string}) {
	const theme = useTheme();
	const {frame: f, live} = useFrame(CELEBRATION_INTERVAL_MS, {animate, frame});
	if (!active) return null;
	if (live && f >= CELEBRATION_FRAME_COUNT) return null;
	const glyph = CELEBRATION_GLYPHS[f % CELEBRATION_GLYPHS.length]!;
	const glyphColor = f % 2 === 0 ? theme.color('judge') : theme.color('success');
	return <Text {...colorProps(glyphColor, true)}>{glyph.repeat(3)} {label} {glyph.repeat(3)}</Text>;
}

// --- Danger flash (abort / rollback) ----------------------------------------

const FLASH_INTERVAL_MS = 140;
const FLASH_FRAME_COUNT = 6; // brief flash, then settles on danger

/** Flashes between danger and the base color a few times, then settles on danger — for an abort or rollback. */
export function DangerFlash({active, children, baseColor, animate, frame}: FrameOptions & {active: boolean; children: React.ReactNode; baseColor?: string | undefined}) {
	const theme = useTheme();
	const {frame: f, live} = useFrame(FLASH_INTERVAL_MS, {animate, frame});
	if (!active) return <Text {...colorProps(baseColor)}>{children}</Text>;
	const settled = !live || f >= FLASH_FRAME_COUNT;
	const showDanger = settled || f % 2 === 0;
	return <Text {...colorProps(showDanger ? theme.color('danger') : baseColor, !settled)}>{children}</Text>;
}

// --- Gene bank double-helix accent ------------------------------------------

const HELIX_INTERVAL_MS = 320;
const HELIX_FRAMES: ReadonlyArray<[string, string]> = [['⟋', '⟍'], ['⟍', '⟋']];

/** A small twisting double-helix accent glyph for the Gene Bank, alternating the champion/challenger colors like paired strands. */
export function GeneHelix({animate, frame}: FrameOptions = {}) {
	const theme = useTheme();
	const {frame: f} = useFrame(HELIX_INTERVAL_MS, {animate, frame});
	const [a, b] = HELIX_FRAMES[f % HELIX_FRAMES.length]!;
	return <Text><Text {...colorProps(theme.color('champion'))}>{a}</Text><Text {...colorProps(theme.color('challenger'))}>{b}</Text></Text>;
}
