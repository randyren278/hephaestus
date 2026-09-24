import React, {useCallback, useEffect, useRef, useState} from 'react';
import {Box, Text, useApp, useInput, useWindowSize} from 'ink';
import {ControlClient} from './client.js';
import {safeText, type ApiResponse, type Command, type ResponseData} from './protocol.js';

const MENU = ['Status', 'Freeze', 'Unfreeze', 'Kill all active work', 'Inspect job by ID', 'Cancel job by ID'] as const;
type View = 'home' | 'job-id' | 'confirm-kill' | 'confirm-kill-all';
type TuiClient = Pick<ControlClient, 'request'>;
type Props = {client?: TuiClient; pollMs?: number};

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
	return 'No response data';
}

function statusOf(data: ResponseData | undefined): string {
	return data?.type === 'status' ? (data.frozen ? 'FROZEN' : 'RUNNING') : '—';
}

export function App({client: providedClient, pollMs = 1500}: Props) {
	const {exit} = useApp();
	const {columns = 80, rows = 24} = useWindowSize();
	const [client] = useState(() => providedClient ?? new ControlClient());
	const [selected, setSelected] = useState(0);
	const [view, setView] = useState<View>('home');
	const [jobId, setJobId] = useState('');
	const [jobPromptAction, setJobPromptAction] = useState<'inspect' | 'cancel'>('inspect');
	const [status, setStatus] = useState<ApiResponse>();
	const [stale, setStale] = useState(false);
	const [notice, setNotice] = useState('Connecting to the local control plane…');
	const [busy, setBusy] = useState(false);
	const refreshing = useRef(false);
	const refresh = useCallback(async (announce = true): Promise<ApiResponse | undefined> => {
		if (refreshing.current) return undefined;
		refreshing.current = true;
		try {
			const response = await client.request({command: 'status'});
			setStatus(response);
			setStale(Boolean(response.error));
			if (announce) setNotice(messageFor(response));
			return response;
		} catch (error) {
			setStale(true);
			if (announce) setNotice(error instanceof Error ? safeText(error.message) : 'Local daemon unavailable');
			return undefined;
		} finally {
			refreshing.current = false;
		}
	}, [client]);
	useEffect(() => {
		void refresh();
		const timer = setInterval(() => void refresh(false), pollMs);
		return () => clearInterval(timer);
	}, [refresh, pollMs]);

	const act = async (command: Command, pending: string) => {
		if (busy) return;
		setBusy(true);
		setNotice(pending);
		try {
			const response = await client.request(command);
			setNotice(messageFor(response));
			if (response.error) return;
			if (command.command === 'job_kill') {
				setNotice(`Cancellation requested for ${safeText(command.job_id)}; waiting for daemon confirmation…`);
				const deadline = Date.now() + 20_000;
				let confirmed = false;
				while (Date.now() < deadline) {
					await new Promise(resolve => setTimeout(resolve, 350));
					const current = await client.request({command: 'job_status', job_id: command.job_id});
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
			setNotice(error instanceof Error ? safeText(error.message) : 'Request failed');
		} finally {
			setBusy(false);
		}
	};

	useInput((input, key) => {
		if (view === 'home' && input.toLowerCase().includes('q')) { exit(); return; }
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
			{!compact && <Box flexDirection="column" width="42%" borderStyle="single" borderColor="gray" paddingX={1}>
				<Text color="gray">CANONICAL STATUS</Text>
				<Text>Active runs  {status?.data?.type === 'status' ? status.data.active_runs : '—'}</Text>
				<Text>Genomes      {status?.data?.type === 'status' ? status.data.genome_count : '—'}</Text>
				<Text>Ledger events {status?.data?.type === 'status' ? status.data.event_count : '—'}</Text>
			</Box>}
		</Box>
		{view !== 'home' && <Box marginTop={1} borderStyle="round" borderColor="yellow" paddingX={1}>
			{view === 'job-id' && <Text>Job ID: {jobId}<Text color="gray">  (Enter {jobPromptAction} · Esc cancel)</Text></Text>}
			{view === 'confirm-kill' && <Text color="yellow">Cancel job {safeText(jobId)}? Press Y to request, N/Esc to back out.</Text>}
			{view === 'confirm-kill-all' && <Text color="red">Cancel ALL active work? Press Y to request, N/Esc to back out.</Text>}
		</Box>}
		<Box flexGrow={1} />
		<Box borderStyle="single" borderColor="gray" paddingX={1}>
			<Text wrap="truncate" color={busy ? 'yellow' : 'white'}>{notice}</Text>
		</Box>
		<Text color="gray">↑↓/JK navigate · Enter select · Y/N confirm · Q quit · daemon remains running</Text>
	</Box>;
}
