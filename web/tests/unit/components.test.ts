import { afterEach, describe, expect, it } from 'vitest';
import { flushSync, mount, unmount, type Component } from 'svelte';
import TreeRow from '../../src/components/TreeRow.svelte';
import Transcript from '../../src/components/Transcript.svelte';
import MessageCard from '../../src/components/MessageCard.svelte';
import { ui } from '../../src/lib/ui.svelte';
import { isUnsafe } from '../../src/lib/visible';

const mounted: Record<string, unknown>[] = [];
function render<P extends Record<string, unknown>>(C: Component<P>, props: P): HTMLElement {
  const target = document.createElement('div');
  document.body.appendChild(target);
  mounted.push(mount(C, { target, props }) as Record<string, unknown>);
  flushSync();
  return target;
}
afterEach(() => {
  for (const m of mounted.splice(0)) unmount(m);
  document.body.innerHTML = '';
});

const now = Date.parse('2026-10-04T12:00:00Z');

describe('TreeRow', () => {
  it('shows state, flags and counts', () => {
    ui.lang = 'en';
    const el = render(TreeRow, {
      row: { name: 'be-lead', depth: 1, hasChildren: true, expanded: false, context: false },
      seat: {
        name: 'be-lead',
        kind: 'agent',
        harness: 'codex',
        model: 'gpt-x',
        created_at: '2026-10-01T00:00:00Z',
        presence: { state: 'awaiting', since: '2026-10-04T11:50:00Z', updated_at: '2026-10-04T11:59:50Z', attention: 'permission', wake_failed: true },
      },
      counts: { seat: 'be-lead', unread: 3, pending: 2, seq: 9 },
      waiting: true,
      below: { busy: 2, needs: 1 },
      size: 4,
      now,
    });
    const text = el.textContent ?? '';
    expect(el.querySelector('[data-state="awaiting"]')).not.toBeNull();
    expect(text).toContain('codex · gpt-x');
    expect(text).toContain('permission prompt');
    expect(text).toContain('wake failed');
    expect(text).toContain('waiting for you');
    expect(text).toContain('10m');
    expect(text).toContain('+2');
    expect(el.querySelector('.below')?.textContent).toContain('2');
  });
  it('shows a stale seat as offline', () => {
    const el = render(TreeRow, {
      row: { name: 'w', depth: 0, hasChildren: false, expanded: false, context: false },
      seat: { name: 'w', kind: 'agent', created_at: 'x', presence: { state: 'busy', since: '2026-10-04T10:00:00Z', updated_at: '2026-10-04T10:00:00Z' } },
      now,
    });
    expect(el.querySelector('[data-state="offline"]')).not.toBeNull();
  });
});

describe('untrusted text', () => {
  const evil = '<img src=x onerror="alert(1)"><b>bold</b>';
  it('renders transcript text as text', () => {
    const el = render(Transcript, {
      status: { state: 'streaming' },
      events: [
        { role: 'assistant', kind: 'text', text: evil },
        { role: 'system', kind: 'notice', text: '[amail] 1 new message' },
        { role: 'assistant', kind: 'tool_call', tool: 'Bash', text: 'ls <dir>' },
        { role: 'tool', kind: 'tool_result', text: 'boom', error: true, truncated: true },
      ],
    });
    expect(el.querySelector('img')).toBeNull();
    expect(el.querySelector('b')).toBeNull();
    expect(el.textContent).toContain(evil);
    expect(el.querySelector('.k-notice')?.textContent).toContain('mail');
    expect(el.querySelector('.k-tool_call .tool')?.textContent).toBe('Bash');
    expect(el.querySelector('.k-tool_result.err')).not.toBeNull();
    expect(el.textContent).toContain('cut');
  });
  it('explains a missing session instead of showing nothing', () => {
    const el = render(Transcript, { status: { state: 'node_offline' }, events: [] });
    expect(el.querySelector('[data-state="node_offline"]')?.textContent).toContain('No node');
  });
  it('renders mail bodies, subjects and meta as text', () => {
    const el = render(MessageCard, {
      message: {
        id: 5,
        thread_id: 5,
        from: 'w1',
        source: 'agent',
        to: ['owner'],
        cc: [],
        subject: '<script>x</script>',
        priority: 'urgent',
        ack_required: true,
        lines: 1,
        created_at: '2026-10-04T11:00:00Z',
        meta: { commit: '0123456789abcdef', branch: 'amail/w1', note: '<i>x</i>' },
        body: evil,
      },
    });
    expect(el.querySelector('img, script, i')).toBeNull();
    expect(el.textContent).toContain(evil);
    expect(el.textContent).toContain('0123456789ab');
    expect(el.textContent).toContain('amail/w1');
  });
  it('shows characters that are not shown as visible escapes', () => {
    ui.lang = 'en';
    const el = render(MessageCard, {
      message: {
        id: 6,
        thread_id: 6,
        from: 'evil@other',
        source: 'external',
        to: ['w1'],
        cc: [],
        subject: 'sub\u202ej\ufe0f',
        priority: 'normal',
        ack_required: false,
        lines: 2,
        created_at: '2026-10-04T11:00:00Z',
        meta: { note: 'a\u{e0041}b' },
        body: 'line one\nhidden\u200b\u{e0101}\u2800 text\tend',
      },
    });
    const text = el.textContent ?? '';
    expect(text).toContain('sub\\u{202e}j\\u{fe0f}');
    expect(text).toContain('a\\u{e0041}b');
    expect(el.querySelector('pre.body')?.textContent).toBe('line one\nhidden\\u{200b}\\u{e0101}\\u{2800} text\tend');
    expect(el.querySelectorAll('.esc').length).toBe(4);
    expect([...text].filter((c) => isUnsafe(c))).toEqual([]);
  });
});
