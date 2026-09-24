import React, {useCallback, useEffect, useRef, useState} from 'react';
import {Box, Text, useApp, useInput, useWindowSize} from 'ink';
import {ControlClient} from './client.js';
import {safeText, type ApiResponse, type ArenaJobProgress, type Command, type ResponseData} from './protocol.js';

const MENU = ['Status', 'Freeze', 'Unfreeze', 'Kill all active work', 'Inspect job by ID', 'Cancel job by ID', 'Arena progress by ID'] as const;
type View = 'home' | 'job-id' | 'arena-id' | 'arena-progress' | 'confirm-kill' | 'confirm-kill-all';
type TuiClient = Pick<ControlClient, 'request'>;
type Props = {client?: TuiClient; pollMs?: number};

function waitForPoll(ms: number, signal: AbortSignal): Promise<void> {
	if (signal.aborted) return Promise.reject(new Error('daemon request aborted'));
	return new Promise((resolve, reject) => {
		const finish = (error?: Error) => {
			clearTimeout(timer);
			signal.removeEventListener('abort', abort);
			if (error) reject(error);
			else resolve();
		};
		const abort = () => finish(new Error('daemon request aborted'));
		const timer = setTimeout(() => finish(), ms);
		signal.addEventListener('abort', abort, {once: true});
	});
}

const arena = [
	'       ▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄▄',
	'     ▄█  ▄▄  ▄▄  ▄▄  ▄▄  █▄',
	'    █▀█  ██  ██  ██  ██  █▀█',
	'   ▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀▀',
];

function messageFor(response: ApiResponse): string {
	if (response.error) return `Daemon: ${safeText(response.error.code)} — ${safeText(response.error.message)}`;
	if (response.data?.type === 'status') {
		const state = response.data;
		return `Ledger event ${state.event_count} · ${state.genome_count} Genomes`;
	}
	if (response.data?.type === 'acknowledged') return `Acknowledged · evolution ${response.data.frozen ? 'frozen' : 'unfrozen'}`;
	if (response.data?.type === 'job') return `${safeText(response.data.job.job_id)} · ${safeText(response.data.job.state)} · ${response.data.progress.trace_events} trace events`;
	if (response.data?.type === 'arena_job') return `${safeText(response.data.job.evaluation_id)} · ${safeText(response.data.job.phase)} · ${response.data.job.completed_trials}/${response.data.job.total_trials} trials`;
	return 'No response data';
}

function statusOf(data: ResponseData | undefined): string {
	return data?.type === 'status' ? (data.frozen ? 'FROZEN' : 'RUNNING') : '—';
}

export function ArenaProgressPanel({job, stale, notice, compact = false}: {job?: ArenaJobProgress | undefined; stale: boolean; notice?: string; compact?: boolean}) {
	if (!job) return <Box flexDirection="column" borderStyle="single" borderColor={stale ? 'red' : 'gray'} paddingX={1}>
		<Text bold color="yellow">ARENA PROGRESS</Text>
		<Text>{stale ? 'STALE · daemon unavailable' : safeText(notice ?? 'Enter an evaluation ID to inspect its durable progress.')}</Text>
		<Text color="gray">The control API provides progress by known evaluation ID; it has no Arena job list.</Text>
	</Box>;
	const ratio = Math.max(0, Math.min(1, job.completed_trials / job.total_trials));
	const filled = Math.round(ratio * 16);
	return <Box flexDirection="column" borderStyle="single" borderColor={stale ? 'red' : 'gray'} paddingX={1}>
		<Text bold color="yellow">ARENA / {safeText(job.evaluation_id)}</Text>
		<Text color={stale ? 'red' : 'white'}>{stale ? 'STALE · ' : ''}{safeText(job.state.toUpperCase())} · {safeText(job.phase.replaceAll('_', ' ').toUpperCase())}</Text>
		<Text>Trials {job.completed_trials}/{job.total_trials}  {`${'█'.repeat(filled)}${'·'.repeat(16 - filled)}`}</Text>
		{!compact && <>
			<Text>Parent    {safeText(job.parent_genome_id)}</Text>
			<Text>Candidate {safeText(job.candidate_genome_id)}</Text>
			{job.evaluation && <Text>Visible score {job.evaluation.parent_visible_correct} → {job.evaluation.candidate_visible_correct} / {job.evaluation.visible_total}</Text>}
		</>}
	</Box>;
}

export function App({client: providedClient, pollMs = 1500}: Props) {
	const {exit} = useApp();
	const {columns = 80, rows = 24} = useWindowSize();
	const [client] = useState(() => providedClient ?? new ControlClient());
	const [selected, setSelected] = useState(0);
	const [view, setView] = useState<View>('home');
	const [jobId, setJobId] = useState('');
	const [jobPromptAction, setJobPromptAction] = useState<'inspect' | 'cancel'>('inspect');
	const [arenaInput, setArenaInput] = useState('');
	const [arenaJobId, setArenaJobId] = useState('');
	const [arenaJob, setArenaJob] = useState<ArenaJobProgress>();
	const [arenaStale, setArenaStale] = useState(false);
	const [arenaNotice, setArenaNotice] = useState('Enter an evaluation ID to inspect its durable progress.');
	const [status, setStatus] = useState<ApiResponse>();
	const [stale, setStale] = useState(false);
	const [notice, setNotice] = useState('Connecting to the local control plane…');
	const [busy, setBusy] = useState(false);
	const [lifetime] = useState(() => new AbortController());
	const closing = useRef(false);
	const refreshing = useRef(false);
	const arenaRefreshing = useRef(false);
	const refresh = useCallback(async (announce = true): Promise<ApiResponse | undefined> => {
		if (closing.current || refreshing.current) return undefined;
		refreshing.current = true;
		try {
			const response = await client.request({command: 'status'}, lifetime.signal);
			setStatus(response);
			setStale(Boolean(response.error));
			if (announce) setNotice(messageFor(response));
			return response;
		} catch (error) {
			if (lifetime.signal.aborted) return undefined;
			setStale(true);
			if (announce) setNotice(error instanceof Error ? safeText(error.message) : 'Local daemon unavailable');
			return undefined;
		} finally {
			refreshing.current = false;
		}
	}, [client, lifetime]);
	useEffect(() => {
		void refresh();
		const timer = setInterval(() => void refresh(false), pollMs);
		return () => clearInterval(timer);
	}, [refresh, pollMs]);
	useEffect(() => () => lifetime.abort(), [lifetime]);

	const act = async (command: Command, pending: string) => {
		if (closing.current || busy) return;
		setBusy(true);
		setNotice(pending);
		try {
			const response = await client.request(command, lifetime.signal);
			setNotice(messageFor(response));
			if (response.error) return;
			if (command.command === 'job_kill') {
				setNotice(`Cancellation requested for ${safeText(command.job_id)}; waiting for daemon confirmation…`);
				const deadline = Date.now() + 20_000;
				let confirmed = false;
				while (Date.now() < deadline) {
					await waitForPoll(350, lifetime.signal);
					const current = await client.request({command: 'job_status', job_id: command.job_id}, lifetime.signal);
					if (current.data?.type === 'job' && ['succeeded', 'failed', 'interrupted'].includes(current.data.job.state)) {
						setNotice(`Daemon confirmed ${safeText(command.job_id)} terminal: ${safeText(current.data.job.state)}${current.data.job.terminal ? ` / ${safeText(current.data.job.terminal)}` : ''}`);
						confirmed = true;
						break;
					}
				}
				if (!confirmed) setNotice(`Cancellation was requested for ${safeText(command.job_id)} but terminal confirmation timed out.`);
			}
			const latest = await refresh(false);
			if (command.command === 'kill_all') {
				const active = latest?.data?.type === 'status' ? latest.data.active_runs : 'unknown';
				setNotice(`Kill-all recorded · ${active} active; terminal states unavailable.`);
			}
		} catch (error) {
			if (lifetime.signal.aborted) return;
			setNotice(error instanceof Error ? safeText(error.message) : 'Request failed');
		} finally {
			if (!lifetime.signal.aborted) setBusy(false);
		}
	};

	const refreshArena = useCallback(async (evaluationId: string) => {
		if (closing.current || !evaluationId || arenaRefreshing.current) return;
		arenaRefreshing.current = true;
		try {
			const response = await client.request({command: 'job_status', job_id: evaluationId}, lifetime.signal);
			if (response.error) {
				setArenaJob(undefined);
				setArenaStale(false);
				setArenaNotice(`${safeText(response.error.code)} · ${safeText(response.error.message)}`);
				return;
			}
			if (response.data?.type !== 'arena_job') {
				setArenaJob(undefined);
				setArenaStale(false);
				setArenaNotice('No Arena progress projection exists for that ID.');
				return;
			}
			setArenaJob(response.data.job);
			setArenaStale(false);
			setArenaNotice('Live progress from the daemon.');
		} catch (error) {
			if (!lifetime.signal.aborted) {
				setArenaStale(true);
				setArenaNotice(error instanceof Error ? safeText(error.message) : 'Arena progress unavailable');
			}
		} finally {
			arenaRefreshing.current = false;
		}
	}, [client, lifetime]);
	useEffect(() => {
		if (view !== 'arena-progress' || !arenaJobId || !arenaJob || ['succeeded', 'failed', 'interrupted'].includes(arenaJob.state)) return;
		const timer = setInterval(() => void refreshArena(arenaJobId), pollMs);
		return () => clearInterval(timer);
	}, [arenaJobId, arenaJob, pollMs, refreshArena, view]);

	const quit = () => {
		if (closing.current) return;
		closing.current = true;
		lifetime.abort();
		exit();
	};

	useInput((input, key) => {
		if (view === 'home' && input.toLowerCase().includes('q')) { quit(); return; }
		if (view === 'job-id') {
			if (key.escape) { setView('home'); return; }
			if (key.return || input.includes('\r') || input.includes('\n')) {
				const typed = input.replace(/[\r\n]/g, '').split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				const cleaned = (jobId + typed).trim().slice(0, 128);
				if (cleaned) {
					setJobId(cleaned);
					if (jobPromptAction === 'cancel') setView('confirm-kill');
					else { void act({command: 'job_status', job_id: cleaned}, 'Looking up job…'); setView('home'); }
				} else setView('home');
				return;
			}
			if (key.backspace || key.delete) setJobId(value => value.slice(0, -1));
			else if (!key.ctrl && !key.meta) {
				const typed = input.split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				if (typed) setJobId(value => (value + typed).slice(0, 128));
			}
			return;
		}
		if (view === 'arena-id') {
			if (key.escape) { setView('home'); return; }
			if (key.return || input.includes('\r') || input.includes('\n')) {
				const typed = input.replace(/[\r\n]/g, '').split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				const cleaned = (arenaInput + typed).trim().slice(0, 128);
				if (cleaned) {
					setArenaJobId(cleaned);
					setArenaJob(undefined);
					setArenaStale(false);
					setArenaNotice('Loading Arena progress…');
					setView('arena-progress');
					void refreshArena(cleaned);
				} else setView('home');
				return;
			}
			if (key.backspace || key.delete) setArenaInput(value => value.slice(0, -1));
			else if (!key.ctrl && !key.meta) {
				const typed = input.split('').filter(char => /^[a-zA-Z0-9._-]$/.test(char)).join('');
				if (typed) setArenaInput(value => (value + typed).slice(0, 128));
			}
			return;
		}
		if (view === 'arena-progress') {
			if (input.toLowerCase().includes('q')) { quit(); return; }
			if (key.escape) { setView('home'); return; }
			if (input.toLowerCase() === 'r') { void refreshArena(arenaJobId); return; }
			return;
		}
		if (view === 'confirm-kill') {
			if (input.toLowerCase() === 'y') { setView('home'); void act({command: 'job_kill', job_id: jobId}, 'Requesting cancellation…'); }
			else if (input.toLowerCase() === 'n' || key.escape) { setView('home'); setNotice('Cancellation abandoned.'); }
			return;
		}
		if (view === 'confirm-kill-all') {
			if (input.toLowerCase() === 'y') { setView('home'); void act({command: 'kill_all'}, 'Requesting cancellation of all active work…'); }
			else if (input.toLowerCase() === 'n' || key.escape) { setView('home'); setNotice('Kill all abandoned.'); }
			return;
		}
		if (key.upArrow || input === 'k') setSelected(value => Math.max(0, value - 1));
		if (key.downArrow || input === 'j') setSelected(value => Math.min(MENU.length - 1, value + 1));
		if (key.return) {
			switch (selected) {
				case 0: void refresh(); break;
				case 1: void act({command: 'freeze'}, 'Applying freeze…'); break;
				case 2: void act({command: 'unfreeze'}, 'Resuming evolution…'); break;
				case 3: setView('confirm-kill-all'); break;
				case 4: setJobPromptAction('inspect'); setView('job-id'); setJobId(''); break;
				case 5: setJobPromptAction('cancel'); setView('job-id'); setJobId(''); break;
				case 6: setArenaInput(''); setArenaJobId(''); setArenaJob(undefined); setArenaStale(false); setArenaNotice('Enter an evaluation ID to inspect its durable progress.'); setView('arena-id'); break;
			}
		}
	});

	const compact = columns < 72 || rows < 20;
	return <Box flexDirection="column" width={Math.max(1, columns)} height={Math.max(1, rows)} paddingX={1}>
		<Box justifyContent="space-between">
			<Text bold color="yellow">HEPHAESTUS <Text color="gray">/ OPERATOR</Text></Text>
			<Text color={stale ? 'red' : statusOf(status?.data) === 'FROZEN' ? 'yellow' : status ? 'green' : 'gray'}>{stale ? '● STALE' : `● ${statusOf(status?.data)}`}</Text>
		</Box>
		<Box marginTop={1} flexDirection="column">
			{!compact && arena.map((line, index) => <Text key={index} color={index === 2 ? 'yellow' : 'gray'}>{line}</Text>)}
			<Text bold color="white">  LOCAL CONTROL · SCHEMA 1 · OWNER SOCKET</Text>
		</Box>
		<Box marginTop={compact ? 0 : 1}>
			<Box flexDirection="column" width={compact ? '100%' : '58%'}>
				<Text color="gray">OPERATOR ACTIONS</Text>
				{MENU.map((label, index) => <Text key={label} color={selected === index ? 'yellow' : 'white'}>{selected === index ? '› ' : '  '}{label}{selected === index ? '  ‹' : ''}</Text>)}
			</Box>
			{!compact && (view === 'arena-progress'
				? <Box flexDirection="column" width="42%"><ArenaProgressPanel job={arenaJob} stale={stale || arenaStale} notice={arenaNotice} /></Box>
				: <Box flexDirection="column" width="42%" borderStyle="single" borderColor="gray" paddingX={1}>
					<Text color="gray">CANONICAL STATUS</Text>
					<Text>Active runs  {status?.data?.type === 'status' ? status.data.active_runs : '—'}</Text>
					<Text>Genomes      {status?.data?.type === 'status' ? status.data.genome_count : '—'}</Text>
					<Text>Ledger events {status?.data?.type === 'status' ? status.data.event_count : '—'}</Text>
				</Box>)}
		</Box>
		{compact && view === 'arena-progress' && <ArenaProgressPanel job={arenaJob} stale={stale || arenaStale} notice={arenaNotice} compact />}
		{view !== 'home' && <Box marginTop={1} borderStyle="round" borderColor="yellow" paddingX={1}>
			{view === 'job-id' && <Text>Job ID: {jobId}<Text color="gray">  (Enter {jobPromptAction} · Esc cancel)</Text></Text>}
			{view === 'arena-id' && <Text>Evaluation ID: {arenaInput}<Text color="gray">  (Enter inspect · Esc back)</Text></Text>}
			{view === 'arena-progress' && <Text color="gray">Arena progress is read-only · Esc back · R refresh</Text>}
			{view === 'confirm-kill' && <Text color="yellow">Cancel job {safeText(jobId)}? Press Y to request, N/Esc to back out.</Text>}
			{view === 'confirm-kill-all' && <Text color="red">Cancel ALL active work? Press Y to request, N/Esc to back out.</Text>}
		</Box>}
		<Box flexGrow={1} />
		<Box borderStyle="single" borderColor="gray" paddingX={1}>
			<Text wrap="truncate" color={busy ? 'yellow' : 'white'}>{notice}</Text>
		</Box>
		<Text color="gray">↑↓/JK navigate · Enter select · {view === 'arena-progress' ? 'Esc back · R refresh' : 'Y/N confirm'} · Q quit</Text>
	</Box>;
}
