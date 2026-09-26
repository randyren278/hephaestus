import React from 'react';
import test from 'node:test';
import assert from 'node:assert/strict';
import {Writable} from 'node:stream';
import {render, renderToString, Text} from 'ink';
import {
	activeTickerListenerCount, CelebrationBurst, CrestClash, DangerFlash, debugRawFrame, GeneHelix, isTickerRunning, ProgressBar, Spinner, TorchFlicker, Typewriter, useFrame,
} from '../src/motion.js';

/** A stdout stand-in that swallows Ink's frames instead of writing to the real terminal, so a live `render()` in a test doesn't interleave ANSI output with the test runner's own reporting. */
function fakeStdout(): Writable & {isTTY: boolean; columns: number; rows: number} {
	const stream = new Writable({write(_chunk, _encoding, callback) { callback(); }}) as Writable & {isTTY: boolean; columns: number; rows: number};
	stream.isTTY = false;
	stream.columns = 80;
	stream.rows = 24;
	return stream;
}

test('TorchFlicker cycles glyph and color deterministically across fixed frames', () => {
	const frame0 = renderToString(<TorchFlicker frame={0} />);
	const frame1 = renderToString(<TorchFlicker frame={1} />);
	const frame2 = renderToString(<TorchFlicker frame={2} />);
	const frame3 = renderToString(<TorchFlicker frame={3} />);
	assert.equal(frame0, '▲');
	assert.equal(frame1, '▲');
	assert.equal(frame2, '◆');
	// The 3-frame cycle repeats.
	assert.equal(frame3, frame0);
});

test('Typewriter reveals text progressively at a fixed frame and holds once fully revealed', () => {
	assert.equal(renderToString(<Typewriter text="HEPHAESTUS" frame={0} />), 'H');
	assert.equal(renderToString(<Typewriter text="HEPHAESTUS" frame={3} />), 'HEPH');
	assert.equal(renderToString(<Typewriter text="HEPHAESTUS" frame={999} />), 'HEPHAESTUS');
});

test('Typewriter renders the full text immediately when animate is false', () => {
	assert.equal(renderToString(<Typewriter text="HEPHAESTUS" animate={false} />), 'HEPHAESTUS');
});

test('Typewriter renders the full text immediately off a TTY with no frame given (the App default)', () => {
	assert.equal(renderToString(<Typewriter text="HEPHAESTUS" />), 'HEPHAESTUS');
});

test('ProgressBar fills the ratio it is given and never exceeds width regardless of frame', () => {
	const empty = renderToString(<ProgressBar ratio={0} width={10} animate={false} />);
	const half = renderToString(<ProgressBar ratio={0.5} width={10} animate={false} />);
	const full = renderToString(<ProgressBar ratio={1} width={10} animate={false} />);
	assert.equal(empty, '·'.repeat(10));
	assert.equal(half, `${'█'.repeat(5)}${'·'.repeat(5)}`);
	assert.equal(full, '█'.repeat(10));
	// Over-range ratios clamp instead of over- or under-filling.
	assert.equal(renderToString(<ProgressBar ratio={2} width={4} animate={false} />), '████');
	assert.equal(renderToString(<ProgressBar ratio={-1} width={4} animate={false} />), '····');
});

test('CrestClash renders a deterministic idle frame including its spark frame', () => {
	const apart = renderToString(<CrestClash frame={0} />);
	const spark = renderToString(<CrestClash frame={3} />);
	assert.match(apart, /◆/);
	assert.match(apart, /▲/);
	assert.match(spark, /✦/);
});

test('CelebrationBurst renders nothing when inactive, something while active, and stops after its frame budget while live', () => {
	assert.equal(renderToString(<CelebrationBurst active={false} frame={0} />), '');
	const early = renderToString(<CelebrationBurst active label="PROMOTED" frame={0} />);
	assert.match(early, /PROMOTED/);
	assert.match(early, /✦|✧/);
	// Past the ~1.2s budget (10 frames at 120ms) the burst is over.
	assert.equal(renderToString(<CelebrationBurst active frame={20} />), '');
});

test('CelebrationBurst holds its resting frame when not live (no timer, no crash)', () => {
	const output = renderToString(<CelebrationBurst active animate={false} />);
	assert.match(output, /PROMOTION/);
});

test('DangerFlash settles on the danger color once its flash frame budget passes', () => {
	const inactive = renderToString(<DangerFlash active={false} baseColor="white" frame={0}><Text>ABORTED</Text></DangerFlash>);
	assert.match(inactive, /ABORTED/);
	const settled = renderToString(<DangerFlash active frame={99}><Text>ABORTED</Text></DangerFlash>);
	assert.match(settled, /ABORTED/);
});

test('GeneHelix alternates its two strand glyphs deterministically', () => {
	assert.equal(renderToString(<GeneHelix frame={0} />), '⟋⟍');
	assert.equal(renderToString(<GeneHelix frame={1} />), '⟍⟋');
	assert.equal(renderToString(<GeneHelix frame={2} />), '⟋⟍');
});

test('Spinner cycles through its braille frames deterministically', () => {
	assert.equal(renderToString(<Spinner frame={0} />), '⠋');
	assert.equal(renderToString(<Spinner frame={1} />), '⠙');
});

test('useFrame never starts a timer for a fixed frame or animate=false, and reports live=false when disabled', () => {
	function FixedProbe() {
		const {frame, live} = useFrame(100, {frame: 5});
		return <Text>{`${frame}:${live}`}</Text>;
	}
	assert.equal(renderToString(<FixedProbe />), '5:true');
	function DisabledProbe() {
		const {frame, live} = useFrame(100, {animate: false});
		return <Text>{`${frame}:${live}`}</Text>;
	}
	assert.equal(renderToString(<DisabledProbe />), '0:false');
	assert.equal(activeTickerListenerCount(), 0);
});

test('useFrame ticks forward over real time when explicitly animated, and its ticker stops on unmount', async () => {
	function LiveProbe() {
		const {frame} = useFrame(50, {animate: true});
		return <Text>{String(frame)}</Text>;
	}
	assert.equal(activeTickerListenerCount(), 0, 'no other test left the shared ticker open');
	const instance = render(<LiveProbe />, {stdout: fakeStdout(), patchConsole: false} as never);
	assert.equal(activeTickerListenerCount(), 1, 'mounting a live consumer opens the shared ticker');
	assert.equal(isTickerRunning(), true);
	const before = debugRawFrame();
	await new Promise(resolve => setTimeout(resolve, 260));
	const after = debugRawFrame();
	instance.unmount();
	await new Promise(resolve => setTimeout(resolve, 10));
	assert.equal(activeTickerListenerCount(), 0, 'unmounting the only live consumer closes the shared ticker');
	assert.equal(isTickerRunning(), false, 'the shared setInterval is cleared once nothing needs it');
	assert.ok(after > before, `expected the shared ticker to have advanced, got ${before} -> ${after}`);
});
