// Starts a real `amail serve` on a free port with a fresh SQLite database,
// creates an org with seats and some mail, and (unless AMAIL_E2E_NODE=0)
// a real `amail node` that hosts seat `be-1`, whose transcript is a fixture
// file. A second org with 1,000 seats is created for the scale screenshot.
// Everything runs under web/.e2e/run and is stopped by the returned teardown.

import { spawn, spawnSync, type ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import net from 'node:net';
import path from 'node:path';
import { AGENTS, ROOT, STATE_FILE, type E2eState } from './env';

const BIN = process.env.AMAIL_BIN ?? path.resolve(ROOT, '../target/debug/amail');
const CLEAR = ['AMAIL_URL', 'AMAIL_TOKEN', 'AMAIL_PROFILE', 'AMAIL_SEAT', 'AMAIL_NODE_SOCK', 'AMAIL_STATE_DIR', 'AMAIL_DB', 'AMAIL_NODE_TOKEN', 'CODEX_HOME', 'CLAUDE_CODE_MESSAGING_SOCKET', 'CLAUDE_PID'];

// zellij's directories for every process started here (the node may run
// zellij): fresh ones, never the person's own sessions, cache, configuration
// or data.
const XDG_ROOT = fs.mkdtempSync(path.join(os.tmpdir(), 'amail-e2e-xdg-'));
const XDG: Record<string, string> = {};
for (const [k, sub] of [['XDG_RUNTIME_DIR', 'run'], ['XDG_CACHE_HOME', 'cache'], ['XDG_CONFIG_HOME', 'config'], ['XDG_DATA_HOME', 'data']]) {
  XDG[k] = path.join(XDG_ROOT, sub);
  fs.mkdirSync(XDG[k], { mode: 0o700 });
}

function env(extra: Record<string, string> = {}): NodeJS.ProcessEnv {
  const e: NodeJS.ProcessEnv = { ...process.env };
  for (const k of CLEAR) delete e[k];
  return { ...e, ...XDG, ...extra };
}

// AMAIL_E2E_PORT_BASE: take ports from there on instead of free ones the
// system picks (to keep to a range).
let nextPort = Number(process.env.AMAIL_E2E_PORT_BASE ?? 0);

async function freePort(): Promise<number> {
  if (nextPort) return nextPort++;
  return new Promise((resolve, reject) => {
    const s = net.createServer();
    s.listen(0, '127.0.0.1', () => {
      const port = (s.address() as net.AddressInfo).port;
      s.close(() => resolve(port));
    });
    s.on('error', reject);
  });
}

function run(args: string[], extraEnv: Record<string, string> = {}, input?: string): string {
  const r = spawnSync(BIN, args, { env: env(extraEnv), input, encoding: 'utf8' });
  if (r.status !== 0) throw new Error(`amail ${args.join(' ')} failed (${r.status}): ${r.stderr}`);
  return r.stdout;
}

async function waitFor(f: () => Promise<boolean>, what: string, ms = 20_000): Promise<void> {
  const end = Date.now() + ms;
  while (Date.now() < end) {
    try {
      if (await f()) return;
    } catch {
      // not yet
    }
    await new Promise((r) => setTimeout(r, 150));
  }
  throw new Error(`timed out waiting for ${what}`);
}

async function call<T>(base: string, token: string, method: string, p: string, body?: unknown): Promise<T> {
  const r = await fetch(`${base}/v1${p}`, {
    method,
    headers: { Authorization: `Bearer ${token}`, 'Content-Type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
  });
  const text = await r.text();
  if (!r.ok) throw new Error(`${method} ${p} -> ${r.status}: ${text}`);
  return (text ? JSON.parse(text) : undefined) as T;
}

/** A Claude Code transcript (hand-written, minimal) for seat be-1. */
export function transcriptLines(): string[] {
  const ts = (s: number) => new Date(Date.now() - (600 - s) * 1000).toISOString();
  const rec = (o: Record<string, unknown>) => JSON.stringify({ sessionId: 'e2e-session', isSidechain: false, ...o });
  return [
    rec({ type: 'user', timestamp: ts(0), message: { role: 'user', content: '[amail] You have new mail (wake #1).' } }),
    rec({ type: 'attachment', timestamp: ts(1), attachment: { type: 'hook_additional_context', content: ['[amail] 1 new message for be-1\n#4 from be-lead: Storage schema\n  > Please add the search index.'], hookEvent: 'UserPromptSubmit' } }),
    rec({ type: 'assistant', timestamp: ts(5), message: { role: 'assistant', content: [{ type: 'text', text: 'I will add the trigram index and a migration.' }] } }),
    rec({ type: 'assistant', timestamp: ts(9), message: { role: 'assistant', content: [{ type: 'tool_use', id: 'tu_1', name: 'Bash', input: { command: 'cargo test -p store search' } }] } }),
    rec({ type: 'user', timestamp: ts(30), message: { role: 'user', content: [{ type: 'tool_result', tool_use_id: 'tu_1', content: 'running 6 tests\ntest result: ok. 6 passed', is_error: false }] } }),
    'this line is not JSON and is skipped',
    rec({ type: 'assistant', timestamp: ts(40), message: { role: 'assistant', content: [{ type: 'text', text: 'Tests pass. <b>Sending</b> the merge request to be-lead.' }] } }),
  ];
}

export default async function globalSetup(): Promise<() => Promise<void>> {
  const procs: ChildProcess[] = [];
  const stop = async () => {
    for (const p of [...procs].reverse()) {
      try {
        p.kill('SIGTERM');
      } catch {
        // already gone
      }
    }
    await new Promise((r) => setTimeout(r, 300));
    for (const p of procs) {
      try {
        if (p.exitCode === null) p.kill('SIGKILL');
      } catch {
        // already gone
      }
    }
  };
  try {
    await setup(procs);
  } catch (e) {
    await stop();
    throw e;
  }
  return stop;
}

async function setup(procs: ChildProcess[]): Promise<void> {
  if (!fs.existsSync(BIN)) throw new Error(`amail binary not found at ${BIN}; build it with cargo build (or set AMAIL_BIN)`);
  const dir = process.env.AMAIL_E2E_DIR ?? path.resolve(ROOT, '.e2e/run');
  fs.rmSync(dir, { recursive: true, force: true });
  fs.mkdirSync(dir, { recursive: true });
  const db = `sqlite://${path.join(dir, 'amail.db')}`;

  const org = 'acme';
  const userToken = (JSON.parse(run(['--json', 'admin', 'create-org', org, '--db', db])) as { token: string }).token;
  const bigToken = (JSON.parse(run(['--json', 'admin', 'create-org', 'scale', '--db', db])) as { token: string }).token;
  // Other orgs: globex (linked with acme), initech (asks acme for a link).
  const peerOrg = 'globex';
  const peerToken = (JSON.parse(run(['--json', 'admin', 'create-org', peerOrg, '--db', db])) as { token: string }).token;
  const inviter = 'initech';
  const inviterToken = (JSON.parse(run(['--json', 'admin', 'create-org', inviter, '--db', db])) as { token: string }).token;

  const port = await freePort();
  const base = `http://127.0.0.1:${port}`;
  const serveLog = fs.openSync(path.join(dir, 'serve.log'), 'a');
  const serve = spawn(BIN, ['serve', '--db', db, '--listen', `127.0.0.1:${port}`], {
    env: env({ RUST_LOG: process.env.RUST_LOG ?? 'info' }),
    stdio: ['ignore', serveLog, serveLog],
  });
  procs.push(serve);
  await waitFor(async () => (await fetch(`${base}/healthz`)).ok, 'amail serve');

  // Seats, tokens and some mail.
  for (const a of AGENTS) await call(base, userToken, 'POST', '/seats', a);
  const seatTokens: Record<string, string> = {};
  for (const a of AGENTS) seatTokens[a.name] = (await call<{ token: string }>(base, userToken, 'POST', `/seats/${a.name}/tokens`, { label: 'e2e' })).token;
  const nodeToken = (await call<{ token: string }>(base, userToken, 'POST', '/tokens', { scope: 'node', label: 'e2e fake presence' })).token;
  await call(base, userToken, 'POST', '/lists', { name: 'leads', description: 'all group leads', post_policy: 'members' });
  for (const m of ['root', 'be-lead', 'fe-lead']) await call(base, userToken, 'PUT', `/lists/leads/members/${m}`, { mode: 'immediate' });

  const kickoff = await call<{ id: number }>(base, userToken, 'POST', '/messages', {
    to: ['root'],
    cc: ['leads'],
    subject: 'Kickoff: full-text search',
    body: 'Add full-text search over the archive.\nIt must handle Chinese substrings.',
  });
  await call(base, seatTokens.root, 'GET', `/messages/${kickoff.id}`);
  await call(base, seatTokens.root, 'POST', '/messages', { to: ['owner'], subject: 'Re: Kickoff: full-text search', body: 'Plan: backend builds the index, frontend the search page. ETA two hours.', in_reply_to: kickoff.id });
  await call(base, seatTokens['be-lead'], 'POST', '/messages', {
    to: ['owner'],
    subject: 'Should short queries scan or fail?',
    body: 'Queries shorter than three characters cannot use the trigram index.\nScan the most recent 2,000 messages, or reject them?',
    priority: 'urgent',
    ack_required: true,
  });
  await call(base, seatTokens['fe-lead'], 'POST', '/messages', {
    to: ['root'],
    cc: ['owner'],
    subject: 'Merge request: search page',
    body: 'Rebased on main, 31 tests pass.',
    meta: { repo: '/work/app', branch: 'amail/fe-lead', commit: '4f9c2e1a7b3d5e6f8091a2b3c4d5e6f708192a3b' },
  });
  await call(base, seatTokens.docs, 'POST', '/messages', {
    to: ['owner'],
    subject: '数据库迁移方案',
    body: '建议先迁移只读副本，再切换主库。迁移期间检索服务保持可用。',
  });

  // The other orgs. globex opens its front desk to every linked org and
  // its eng team (contact: eng-lead) to acme; quiet and eng-1 stay closed.
  await call(base, peerToken, 'PATCH', '/org', { display_name: 'Globex Corporation', description: 'Builds the billing platform. Ask the front desk first.' });
  const peerSeats = [
    { name: 'help', description: 'Front desk of Globex: questions about the billing API.' },
    { name: 'eng-lead', group: 'eng', contact: true, description: 'Leads billing engineering.' },
    { name: 'eng-1', parent: 'eng-lead', group: 'eng', description: 'Billing engineer.' },
    { name: 'quiet', description: 'Internal reviewer.' },
  ];
  for (const x of peerSeats) await call(base, peerToken, 'POST', '/seats', { harness: 'claude', model: 'sonnet', ...x });
  await call(base, peerToken, 'PUT', '/teams/eng', { description: 'Billing engineering.', root: 'eng-lead' });
  const peerSeatTokens: Record<string, string> = {};
  for (const x of peerSeats) peerSeatTokens[x.name] = (await call<{ token: string }>(base, peerToken, 'POST', `/seats/${x.name}/tokens`, { label: 'e2e' })).token;
  await call(base, userToken, 'POST', '/links', { org: peerOrg, note: 'acme here: let us coordinate the search API.' });
  await call(base, peerToken, 'POST', `/links/${org}/accept`);
  await call(base, peerToken, 'POST', '/grants', { target: 'help' });
  await call(base, peerToken, 'POST', '/grants', { target: 'eng', audience: org });
  await call(base, inviterToken, 'POST', '/links', { org, note: 'Initech would like to exchange release notes.' });
  // Mail from globex: to acme's user with closed globex seats in To and Cc
  // (acme sees a count), and from a closed seat that thereby becomes a
  // participant of its thread.
  await call(base, peerSeatTokens.help, 'POST', '/messages', {
    to: [`owner@${org}`, 'quiet'],
    cc: ['eng-1'],
    subject: 'Billing API questions',
    body: 'Which currency fields does your search index need?',
  });
  await call(base, peerSeatTokens.quiet, 'POST', '/messages', {
    to: [`owner@${org}`],
    subject: 'Review notes on the export format',
    body: 'Two small remarks on the export format; answer here if you like.',
  });

  // A node that hosts be-1, whose transcript is a fixture file. Claude Code
  // keeps transcripts under `<config dir>/projects/`, the only place the node
  // follows them from.
  const claudeDir = path.join(dir, 'claude-config');
  fs.mkdirSync(path.join(claudeDir, 'projects', 'e2e'), { recursive: true });
  const transcript = path.join(claudeDir, 'projects', 'e2e', 'be-1-session.jsonl');
  fs.writeFileSync(transcript, transcriptLines().join('\n') + '\n');
  let nodeOk = false;
  if (process.env.AMAIL_E2E_NODE !== '0') {
    const realNodeToken = (await call<{ token: string }>(base, userToken, 'POST', '/tokens', { scope: 'node', label: 'e2e node' })).token;
    const sock = path.join(dir, 'node.sock');
    // Stand-in for claude/codex: never starts a model. Started by
    // `amail run` as a session, it sends the hooks listed in $E2E_HOOKS (the
    // node answers hooks only from the session of the seat they name) and
    // then stays alive; anything else (version checks) exits at once.
    const fake = path.join(dir, 'fake-harness');
    fs.writeFileSync(
      fake,
      [
        '#!/bin/sh',
        '[ -n "$E2E_HOOKS" ] || exit 0',
        'export CLAUDE_PID=$$',
        'while IFS= read -r line; do printf \'%s\\n\' "$line" | "$E2E_AMAIL" hook claude >/dev/null 2>&1; done < "$E2E_HOOKS"',
        ': > "$E2E_HOOKS.done"',
        'exec sleep 3600',
        '',
      ].join('\n'),
      { mode: 0o755 },
    );
    const nodeLog = fs.openSync(path.join(dir, 'node.log'), 'a');
    const node = spawn(
      BIN,
      ['node', '--url', base, '--token', realNodeToken, '--state-dir', path.join(dir, 'node-state'), '--socket', sock, '--label', 'e2e-host', '--claude-bin', fake, '--codex-bin', fake, '--codex-auth', path.join(dir, 'no-auth.json')],
      { env: env({ RUST_LOG: 'info' }), stdio: ['ignore', nodeLog, nodeLog] },
    );
    procs.push(node);
    await waitFor(async () => fs.existsSync(sock), 'amail node socket');
    const hookInput = (o: Record<string, unknown>) => JSON.stringify({ session_id: 'e2e-session', transcript_path: transcript, cwd: dir, ...o });
    const hooks = path.join(dir, 'be-1-hooks.jsonl');
    fs.writeFileSync(
      hooks,
      [
        hookInput({ hook_event_name: 'SessionStart', source: 'startup', model: 'sonnet' }),
        hookInput({ hook_event_name: 'UserPromptSubmit', prompt: '[amail] You have new mail (wake #1).', prompt_id: 'p1', permission_mode: 'bypassPermissions' }),
        '',
      ].join('\n'),
    );
    // The session: `amail run` asks the node for the launch plan and becomes
    // the stand-in harness (its pid keeps the seat alive).
    const session = spawn(BIN, ['run', 'be-1', '--harness', 'claude'], {
      cwd: dir,
      env: env({
        AMAIL_NODE_SOCK: sock,
        CLAUDE_CONFIG_DIR: claudeDir,
        CLAUDE_CODE_MESSAGING_SOCKET: path.join(dir, 'no-inbox.sock'),
        AMAIL_HOOK_TIMEOUT_MS: '5000',
        E2E_HOOKS: hooks,
        E2E_AMAIL: BIN,
      }),
      stdio: ['ignore', nodeLog, nodeLog],
    });
    procs.push(session);
    await waitFor(async () => fs.existsSync(hooks + '.done'), 'the stand-in session to send its hooks');
    nodeOk = true;
  }

  // The 1,000-seat org: a chief and 37 teams of a lead and 26 workers.
  const bigNode = (await call<{ token: string }>(base, bigToken, 'POST', '/tokens', { scope: 'node', label: 'scale presence' })).token;
  const big: { name: string; parent?: string; harness: string; model: string; group?: string }[] = [{ name: 'chief', harness: 'claude', model: 'sonnet' }];
  for (let g = 0; g < 37; g++) {
    const lead = `team${String(g).padStart(2, '0')}-lead`;
    big.push({ name: lead, parent: 'chief', harness: g % 3 ? 'claude' : 'codex', model: g % 3 ? 'sonnet' : 'codex-model', group: `team${g}` });
    for (let w = 1; w <= 26; w++) big.push({ name: `team${String(g).padStart(2, '0')}-w${w}`, parent: lead, harness: 'claude', model: 'sonnet', group: `team${g}` });
  }
  // Parents first: the chief, then the leads, then the workers in parallel batches.
  const chunk = 25;
  for (const level of [big.filter((x) => !x.parent), big.filter((x) => x.parent === 'chief'), big.filter((x) => x.parent && x.parent !== 'chief')]) {
    for (let i = 0; i < level.length; i += chunk) {
      await Promise.all(level.slice(i, i + chunk).map((x) => call(base, bigToken, 'POST', '/seats', x)));
    }
  }

  // A second server in public mode (registration, operator view). Reached
  // as localhost so that the browser keeps its always-Secure cookie.
  const pubDb = `sqlite://${path.join(dir, 'public.db')}`;
  const invites = (JSON.parse(run(['--json', 'admin', 'invite', 'create', '--count', '4', '--note', 'e2e', '--db', pubDb])) as { invites: { code: string }[] }).invites.map((i) => i.code);
  const opToken = (JSON.parse(run(['--json', 'admin', 'operator-token', 'create', '--label', 'e2e', '--db', pubDb])) as { token: string }).token;
  const pubPort = await freePort();
  const pubBase = `http://localhost:${pubPort}`;
  const pubLog = fs.openSync(path.join(dir, 'serve-public.log'), 'a');
  const pub = spawn(BIN, ['serve', '--public', '--db', pubDb, '--listen', `127.0.0.1:${pubPort}`, '--retention-days', '90'], {
    env: env({ RUST_LOG: process.env.RUST_LOG ?? 'info' }),
    stdio: ['ignore', pubLog, pubLog],
  });
  procs.push(pub);
  await waitFor(async () => (await fetch(`http://127.0.0.1:${pubPort}/healthz`)).ok, 'amail serve --public');

  const st: E2eState = {
    base,
    dir,
    org,
    userToken,
    nodeToken,
    seatTokens,
    transcript,
    node: nodeOk,
    big: { org: 'scale', userToken: bigToken, nodeToken: bigNode },
    peer: { org: peerOrg, userToken: peerToken, seatTokens: peerSeatTokens, inviter },
    pub: { base: pubBase, invites, opToken, db: pubDb },
    pids: procs.map((p) => p.pid!).filter(Boolean),
  };
  fs.writeFileSync(STATE_FILE, JSON.stringify(st, null, 2));
}
