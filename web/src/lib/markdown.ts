// Document bodies as HTML: Markdown rendered with `marked`, sanitized with
// DOMPurify (documents are written by agents: no script, no event handler,
// no `javascript:` link survives), headings given the server's section
// anchors, and references to documents and tasks turned into links inside
// the app.

import { Marked } from 'marked';
import DOMPurify from 'dompurify';
import { docHref, headedSections, slugify } from './docs';
import type { DocSection } from './types';

const marked = new Marked({ gfm: true, breaks: false });

// Tags and attributes that a document body may not carry. Images are left
// out (an image loads a URL as soon as the page is shown); forms and
// embedded content have no place in a document; ids and classes would
// collide with the app's own.
const PURIFY = {
  FORBID_TAGS: [
    'style',
    'img',
    'picture',
    'source',
    'video',
    'audio',
    'iframe',
    'frame',
    'object',
    'embed',
    'form',
    'button',
    'textarea',
    'select',
    'option',
    'svg',
    'math',
    'link',
    'meta',
    'base',
    'template',
  ],
  FORBID_ATTR: ['style', 'id', 'name', 'class', 'srcset', 'action', 'formaction', 'target', 'ping', 'background', 'poster'],
  ALLOW_DATA_ATTR: false,
  ALLOW_ARIA_ATTR: false,
  // http(s), mailto, `doc:` (a document link the server reads) and
  // relative URLs; no other scheme.
  ALLOWED_URI_REGEXP: /^(?:(?:https?|mailto|doc):|[^a-z]|[a-z+.-]+(?:[^a-z+.\-:]|$))/i,
};

/** A YAML front matter block at the start of a body (`---` … `---`), kept
 * apart: the server reads it as metadata, not as text. */
export function splitFrontMatter(body: string): { front: string | null; rest: string } {
  const m = /^---[ \t]*\r?\n([\s\S]*?)\r?\n(?:---|\.\.\.)[ \t]*(?:\r?\n|$)/.exec(body);
  if (!m) return { front: null, rest: body };
  return { front: m[1], rest: body.slice(m[0].length) };
}

/** ATX headings lose a trailing attribute block (`# Rollback {#rollback}`),
 * as the server's parser reads it; code blocks are left alone. */
export function stripHeadingAttributes(md: string): string {
  let fence: string | null = null;
  return md
    .split('\n')
    .map((line) => {
      const f = /^ {0,3}(`{3,}|~{3,})/.exec(line);
      if (f) {
        if (fence === null) fence = f[1][0].repeat(f[1].length);
        else if (f[1].startsWith(fence)) fence = null;
        return line;
      }
      if (fence !== null) return line;
      if (!/^ {0,3}#{1,6}\s/.test(line)) return line;
      return line.replace(/\s*\{\s*(?:[#.][^{}]*|[^{}]*=[^{}]*)\}\s*$/, '');
    })
    .join('\n');
}

/** Where a link of a body points in the app: a document (`d12`,
 * `d12#rollback`, `doc:12`, `#anchor` in this document), a task (`t3`), an
 * external URL (`http(s):`, `mailto:`), or nowhere (null: a relative path
 * the browser cannot resolve, or a scheme that is not allowed). */
export function rewriteHref(raw: string, current: number | null): { href: string; external: boolean } | null {
  const h = raw.trim();
  if (!h) return null;
  if (/^(https?:|mailto:)/i.test(h)) return { href: h, external: true };
  if (h.startsWith('#/')) return { href: h, external: false };
  if (h.startsWith('#')) {
    if (current === null) return null;
    return { href: docHref(current, safeDecode(h.slice(1))), external: false };
  }
  const rest = h.replace(/^doc:/i, '');
  const d = /^[dD]?(\d{1,15})(?:#(.*))?$/.exec(rest);
  if (d && (rest !== h || /^[dD]/.test(rest))) {
    const n = Number(d[1]);
    if (n > 0) return { href: docHref(n, d[2] ? safeDecode(d[2]) : undefined), external: false };
  }
  const t = /^[tT](\d{1,15})$/.exec(h);
  if (t && Number(t[1]) > 0) return { href: `#/tasks/t${Number(t[1])}`, external: false };
  return null;
}

function safeDecode(s: string): string {
  try {
    return decodeURIComponent(s);
  } catch {
    return s;
  }
}

/** References written as plain text, as the server reads them: `d12`,
 * `d12#anchor`, `t3`, `thread 7` (not inside words or paths). */
const REF = /(?<![\p{L}\p{N}_@/.#-])(?:[dD](\d{1,15})(?:#([\p{L}\p{N}_-]*[\p{L}\p{N}]))?|[tT](\d{1,15})|[tT]hread (\d{1,15}))(?![\p{L}\p{N}_])/gu;

/** Split text at references: the plain parts and the links. */
export function linkRefs(text: string): ({ text: string } | { text: string; href: string })[] {
  const out: ({ text: string } | { text: string; href: string })[] = [];
  let last = 0;
  for (const m of text.matchAll(REF)) {
    const i = m.index ?? 0;
    let href: string;
    if (m[1]) href = docHref(Number(m[1]), m[2]);
    else if (m[3]) href = `#/tasks/t${Number(m[3])}`;
    else href = `#/thread/${Number(m[4])}`;
    if (i > last) out.push({ text: text.slice(last, i) });
    out.push({ text: m[0], href });
    last = i + m[0].length;
  }
  if (last < text.length) out.push({ text: text.slice(last) });
  return out;
}

const DROP = new Set(['script', 'noscript', ...PURIFY.FORBID_TAGS]);
const URL_ATTRS = ['href', 'src', 'xlink:href', 'action', 'formaction', 'cite', 'data'];

/** A second pass over what DOMPurify left (it relies on the browser's
 * parser; this does not): no element that runs or loads anything, no event
 * handler or style attribute, and only `http(s):`, `mailto:` or relative
 * URLs. */
function scrub(root: DocumentFragment) {
  for (const el of [...root.querySelectorAll('*')]) {
    if (DROP.has(el.localName)) {
      el.remove();
      continue;
    }
    for (const attr of [...el.attributes]) {
      const name = attr.name.toLowerCase();
      if (name.startsWith('on') || PURIFY.FORBID_ATTR.includes(name)) el.removeAttribute(attr.name);
      else if (URL_ATTRS.includes(name)) {
        const v = [...attr.value].filter((c) => c.charCodeAt(0) > 0x20).join('');
        if (/^[a-z][a-z0-9+.-]*:/i.test(v) && !/^(https?|mailto|doc):/i.test(v)) el.removeAttribute(attr.name);
      }
    }
  }
}

export interface Rendered {
  /** The body's nodes, to be inserted into the page as they are. */
  fragment: DocumentFragment;
  /** The front matter, shown apart as text. */
  front: string | null;
  /** The anchors given to the headings, in body order. */
  anchors: string[];
}

/** Render a body. `sections` is the server's section list of the same
 * revision: heading `i` of the body gets the anchor of the server's
 * heading `i` (when the counts differ, a heading takes the anchor of the
 * section with its text, else a slug of its text). `current` is the
 * document's number (for `#anchor` links). */
export function renderDoc(body: string, sections: DocSection[], current: number | null): Rendered {
  const { front, rest } = splitFrontMatter(body);
  const raw = marked.parse(stripHeadingAttributes(rest), { async: false }) as string;
  const clean = DOMPurify.sanitize(raw, PURIFY) as unknown as string;
  const tpl = document.createElement('template');
  tpl.innerHTML = clean;
  const root = tpl.content;
  scrub(root);

  // Headings: the server's anchors.
  const heads = [...root.querySelectorAll<HTMLElement>('h1, h2, h3, h4, h5, h6')];
  const known = headedSections(sections);
  const used = new Set<string>();
  const anchors: string[] = [];
  heads.forEach((h, i) => {
    const text = (h.textContent ?? '').trim();
    let anchor: string | undefined;
    if (known.length === heads.length) anchor = known[i].anchor;
    else anchor = known.find((s) => !used.has(s.anchor) && s.heading === text)?.anchor;
    if (!anchor) {
      const base = slugify(text) || 'section';
      anchor = base;
      for (let n = 1; used.has(anchor); n++) anchor = `${base}-${n}`;
    }
    used.add(anchor);
    anchors.push(anchor);
    h.id = anchor;
    h.setAttribute('data-anchor', anchor);
    if (current !== null) {
      const a = document.createElement('a');
      a.className = 'hlink';
      a.setAttribute('href', docHref(current, anchor));
      a.textContent = '#';
      a.setAttribute('aria-hidden', 'true');
      a.tabIndex = -1;
      h.appendChild(a);
    }
  });

  // Links: in the app, outside it (a new tab), or plain text.
  for (const a of [...root.querySelectorAll<HTMLAnchorElement>('a[href]')]) {
    if (a.classList.contains('hlink')) continue;
    const target = rewriteHref(a.getAttribute('href') ?? '', current);
    if (!target) {
      const span = document.createElement('span');
      span.className = 'deadlink';
      const h = a.getAttribute('href') ?? '';
      if (!h.includes(':')) span.title = h;
      while (a.firstChild) span.appendChild(a.firstChild);
      a.replaceWith(span);
      continue;
    }
    a.setAttribute('href', target.href);
    if (target.external) {
      a.setAttribute('target', '_blank');
      a.setAttribute('rel', 'noopener noreferrer nofollow');
    } else {
      a.removeAttribute('target');
    }
  }
  // Anchors without a target are not links.
  for (const a of [...root.querySelectorAll<HTMLAnchorElement>('a:not([href])')]) {
    const span = document.createElement('span');
    while (a.firstChild) span.appendChild(a.firstChild);
    a.replaceWith(span);
  }

  // Task list boxes are shown, not used.
  for (const input of [...root.querySelectorAll('input')]) {
    if (input.getAttribute('type') !== 'checkbox') input.remove();
    else input.setAttribute('disabled', '');
  }

  // References in text.
  const walker = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
  const texts: Text[] = [];
  for (let n = walker.nextNode(); n; n = walker.nextNode()) {
    const p = (n as Text).parentElement;
    if (p && p.closest('a, code, pre')) continue;
    if (/[dDtT]\d|[tT]hread \d/.test(n.nodeValue ?? '')) texts.push(n as Text);
  }
  for (const n of texts) {
    const parts = linkRefs(n.nodeValue ?? '');
    if (parts.length === 1 && !('href' in parts[0])) continue;
    const frag = document.createDocumentFragment();
    for (const p of parts) {
      if ('href' in p) {
        const a = document.createElement('a');
        a.setAttribute('href', p.href);
        a.textContent = p.text;
        frag.appendChild(a);
      } else frag.appendChild(document.createTextNode(p.text));
    }
    n.replaceWith(frag);
  }

  return { fragment: root, front, anchors };
}

/** The rendered body as HTML text (for tests and checks). */
export function renderHtml(body: string, sections: DocSection[], current: number | null): string {
  const div = document.createElement('div');
  div.appendChild(renderDoc(body, sections, current).fragment);
  return div.innerHTML;
}
