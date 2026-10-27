// @vitest-environment jsdom
// jsdom: DOMPurify needs a browser's HTML parser, which happy-dom does not match.
import { describe, expect, it } from 'vitest';
import { linkRefs, renderDoc, renderHtml, rewriteHref, splitFrontMatter, stripHeadingAttributes } from '../../src/lib/markdown';
import {
  apiRef,
  bodyTemplate,
  diffLines,
  docErrorCode,
  docHref,
  filterQuery,
  filtersFrom,
  handoverTemplate,
  parseDocNum,
  parseDocTarget,
  parseList,
  slugify,
  summarize,
} from '../../src/lib/docs';
import { parseHash } from '../../src/lib/router.svelte';
import type { Doc, DocSection } from '../../src/lib/types';

const sec = (anchor: string, level: number, heading: string, start: number): DocSection => ({ anchor, level, heading, path: heading, start, end: start + 1, tokens: 1 });

function dom(html: string): HTMLDivElement {
  const d = document.createElement('div');
  d.innerHTML = html;
  return d;
}

describe('document ids and targets', () => {
  it('parses numbers, slugs and anchors', () => {
    expect(parseDocNum('d12')).toBe(12);
    expect(parseDocNum('12')).toBe(12);
    expect(parseDocNum('D7')).toBe(7);
    expect(parseDocNum('d0')).toBeNull();
    expect(parseDocNum('runbook')).toBeNull();
    expect(parseDocTarget('d12#rollback')).toEqual({ ref: 'd12', num: 12, anchor: 'rollback' });
    expect(parseDocTarget('Runbook.Deploy')).toEqual({ ref: 'runbook.deploy', num: null, anchor: '' });
    expect(apiRef(parseDocTarget('d12'))).toBe('12');
    expect(apiRef(parseDocTarget('runbook.deploy#steps'))).toBe('runbook.deploy');
  });
  it('builds hash routes that the router reads back', () => {
    expect(docHref(12)).toBe('#/docs/d12');
    expect(docHref(12, 'rollback')).toBe('#/docs/d12#rollback');
    const r = parseHash(docHref(12, '回滚 步骤'));
    expect(r.parts).toEqual(['docs', 'd12#回滚 步骤']);
    expect(parseDocTarget(r.parts[1]).anchor).toBe('回滚 步骤');
    // `href()` encodes the `#`; it decodes the same way.
    expect(parseHash('#/docs/d12%23steps').parts[1]).toBe('d12#steps');
  });
  it('derives slugs as the server does', () => {
    expect(slugify('Deploy > Rollback')).toBe('deploy--rollback');
    expect(slugify('  When to run it? ')).toBe('when-to-run-it');
    expect(slugify('回滚 步骤')).toBe('回滚-步骤');
    expect(slugify('snake_case-and-dash')).toBe('snake_case-and-dash');
  });
  it('helps the forms and lists', () => {
    expect(bodyTemplate(['Context', 'Decision'])).toBe('## Context\n\n## Decision\n\n');
    expect(parseList('a, b  c,,a')).toEqual(['a', 'b', 'c']);
    const f = filtersFrom(new URLSearchParams('kind=runbook&stale=1&team=backend'));
    expect(f).toMatchObject({ kind: 'runbook', stale: true, team: 'backend', status: '' });
    expect(filterQuery(f)).toEqual({ kind: 'runbook', status: undefined, team: 'backend', owner: undefined, label: undefined, stale: '1', pinned: undefined });
    expect(handoverTemplate()).toContain('## Waiting on');
    expect(docErrorCode(409, 'stale')).toBe('stale');
    expect(docErrorCode(404, 'http')).toBe('not_found');
    expect(docErrorCode(500, 'boom')).toBeNull();
  });
  it('summarizes a team', () => {
    const d = (num: number, kind: string, extra: Partial<Doc> = {}): Doc =>
      ({ num, kind, title: `d${num}`, status: 'active', owner: 'w', rev: 1, bytes: 1, tokens: 1, sha256: '', created_at: '', updated_at: '', ...extra }) as Doc;
    const s = summarize([d(1, 'runbook', { pinned: true }), d(2, 'runbook', { stale: true }), d(3, 'decision', { status: 'draft' })]);
    expect(s.pinned.map((x) => x.num)).toEqual([1]);
    expect(s.kinds.get('runbook')).toBe(2);
    expect(s.stale).toBe(1);
    expect(s.drafts).toBe(1);
  });
  it('splits a diff into lines', () => {
    const l = diffLines('title: a -> b\n--- d1 rev 1\n+++ d1 rev 2\n@@ -1 +1 @@\n-old\n+new\n same\n');
    expect(l.map((x) => x.kind)).toEqual(['head', 'head', 'head', 'hunk', 'del', 'add', 'ctx']);
  });
});

describe('Markdown rendering', () => {
  it('keeps the front matter apart and strips heading attributes outside code', () => {
    expect(splitFrontMatter('---\nowner: w1\n---\n# A\n')).toEqual({ front: 'owner: w1', rest: '# A\n' });
    expect(splitFrontMatter('# A\n---\n').front).toBeNull();
    expect(stripHeadingAttributes('## Rollback {#rollback}\n```\n# x {#y}\n```\n')).toBe('## Rollback\n```\n# x {#y}\n```\n');
  });

  it('gives headings the server anchors, in order', () => {
    const body = '# Deploy\n\nintro\n\n## Rollback {#rb}\n\ntext\n\n## Rollback\n';
    const r = renderDoc(body, [sec('deploy', 1, 'Deploy', 0), sec('rb', 2, 'Rollback', 20), sec('rollback', 2, 'Rollback', 40)], 12);
    expect(r.anchors).toEqual(['deploy', 'rb', 'rollback']);
    const d = document.createElement('div');
    d.appendChild(r.fragment);
    expect(d.querySelector('h2')?.id).toBe('rb');
    expect(d.querySelector('h2')?.textContent).toContain('Rollback');
    expect(d.querySelector('h2')?.textContent).not.toContain('{#rb}');
    expect(d.querySelector('h2 a.hlink')?.getAttribute('href')).toBe('#/docs/d12#rb');
  });

  it('falls back to slugs when the section list does not match', () => {
    const r = renderDoc('# One\n\n# One\n\n# Two\n', [], 3);
    expect(r.anchors).toEqual(['one', 'one-1', 'two']);
  });

  it('removes scripts, event handlers and javascript: links', () => {
    const body = [
      '<script>alert(1)</script>',
      '<p onclick="alert(2)" style="color:red" id="main">x</p>',
      '[a](javascript:alert(3))',
      '<a href="javascript:alert(4)">b</a>',
      '<img src=x onerror="alert(5)">',
      '<iframe src="https://example.com"></iframe>',
      '<svg><script>alert(6)</script></svg>',
      '[c](JaVaScRiPt:alert(7))',
      '<a href="data:text/html,<script>alert(8)</script>">d</a>',
      '<form action="https://evil"><input name=q></form>',
    ].join('\n\n');
    const html = renderHtml(body, [], 1);
    const lower = html.toLowerCase();
    expect(lower).not.toContain('<script');
    expect(lower).not.toContain('onclick');
    expect(lower).not.toContain('onerror');
    expect(lower).not.toContain('javascript:');
    expect(lower).not.toContain('data:text');
    expect(lower).not.toContain('<iframe');
    expect(lower).not.toContain('<img');
    expect(lower).not.toContain('<form');
    expect(lower).not.toContain('style=');
    expect(lower).not.toContain('id="main"');
    const d = dom(html);
    for (const a of d.querySelectorAll('a')) expect(a.getAttribute('href') ?? '').toMatch(/^(#\/|https?:|mailto:)/);
  });

  it('turns document and task references into links in the app', () => {
    const body = 'See [the steps](d12#steps), [t](t3), [doc](doc:7), [up](#intro), [[d5]], d9#roll-back and t4 and thread 8.\n\n' +
      'Not in words: abc-d12, docs/d12.md, `d13` in code, [ext](https://example.com/x), [rel](src/main.rs).';
    const d = dom(renderHtml(body, [], 2));
    const hrefs = [...d.querySelectorAll('a')].map((a) => a.getAttribute('href'));
    expect(hrefs).toEqual([
      '#/docs/d12#steps',
      '#/tasks/t3',
      '#/docs/d7',
      '#/docs/d2#intro',
      '#/docs/d5',
      '#/docs/d9#roll-back',
      '#/tasks/t4',
      '#/thread/8',
      'https://example.com/x',
    ]);
    const ext = d.querySelector('a[href^="https"]');
    expect(ext?.getAttribute('target')).toBe('_blank');
    expect(ext?.getAttribute('rel')).toContain('noopener');
    expect(d.querySelector('.deadlink')?.textContent).toBe('rel');
    expect(d.querySelector('code')?.textContent).toBe('d13');
  });

  it('rewrites single hrefs', () => {
    expect(rewriteHref('d12', 1)).toEqual({ href: '#/docs/d12', external: false });
    expect(rewriteHref('doc:d4#x', 1)).toEqual({ href: '#/docs/d4#x', external: false });
    expect(rewriteHref('#a%20b', 6)).toEqual({ href: '#/docs/d6#a%20b', external: false });
    expect(rewriteHref('12', 1)).toBeNull();
    expect(rewriteHref('vbscript:x', 1)).toBeNull();
    expect(rewriteHref('mailto:a@b.c', 1)?.external).toBe(true);
    expect(linkRefs('d1 xd2 t3')).toEqual([{ text: 'd1', href: '#/docs/d1' }, { text: ' xd2 ' }, { text: 't3', href: '#/tasks/t3' }]);
  });

  it('shows task list boxes disabled', () => {
    const d = dom(renderHtml('- [x] done\n- [ ] open\n', [], 1));
    const boxes = d.querySelectorAll('input');
    expect(boxes.length).toBe(2);
    for (const b of boxes) expect(b.hasAttribute('disabled')).toBe(true);
  });
});
