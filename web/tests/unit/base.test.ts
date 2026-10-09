import { afterEach, describe, expect, it, vi } from 'vitest';
import { apiUrl, currentPage, pageUrl, resolveBase } from '../../src/lib/base';

describe('path prefix', () => {
  it('takes the prefix from the meta tag the server writes', () => {
    expect(resolveBase('/amail/', '/amail/register')).toBe('/amail/');
    expect(resolveBase('/', '/')).toBe('/');
    expect(resolveBase('/tools/amail/', '/whatever')).toBe('/tools/amail/');
  });

  it('falls back to the page path without a valid tag', () => {
    for (const bad of [null, undefined, '', 'amail/', '/amail', '//evil/']) {
      expect(resolveBase(bad, '/')).toBe('/');
      expect(resolveBase(bad, '/register')).toBe('/');
      expect(resolveBase(bad, '/admin')).toBe('/');
      expect(resolveBase(bad, '/amail/')).toBe('/amail/');
      expect(resolveBase(bad, '/amail/register')).toBe('/amail/');
      expect(resolveBase(bad, '/amail/admin')).toBe('/amail/');
      expect(resolveBase(bad, '/amail')).toBe('/amail/');
    }
    expect(resolveBase(null, '//x')).toBe('/');
  });

  it('builds API and page URLs below the prefix', () => {
    expect(apiUrl('/v1/events', '/')).toBe('/v1/events');
    expect(apiUrl('/v1/events', '/amail/')).toBe('/amail/v1/events');
    expect(apiUrl('/v1/messages/1?peek=1', '/a/b/')).toBe('/a/b/v1/messages/1?peek=1');
    expect(pageUrl('', '/amail/')).toBe('/amail/');
    expect(pageUrl('register', '/amail/')).toBe('/amail/register');
    expect(pageUrl('admin', '/')).toBe('/admin');
  });

  it('tells which page is open', () => {
    expect(currentPage('/amail/register', '/amail/')).toBe('register');
    expect(currentPage('/amail/admin', '/amail/')).toBe('admin');
    expect(currentPage('/amail/', '/amail/')).toBe('');
    expect(currentPage('/register', '/')).toBe('register');
    expect(currentPage('/', '/')).toBe('');
    expect(currentPage('/amail/other', '/amail/')).toBe('');
  });
});

describe('API client under a prefix', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.resetModules();
    document.head.innerHTML = '';
  });

  it('sends requests below the prefix from the meta tag', async () => {
    document.head.innerHTML = '<meta name="amail-base" content="/amail/" />';
    vi.resetModules();
    const calls: string[] = [];
    vi.stubGlobal(
      'fetch',
      vi.fn(async (url: string) => {
        calls.push(url);
        return new Response('{"ok":true}', { status: 200 });
      }),
    );
    const api = await import('../../src/lib/api');
    await api.get('/v1/info');
    await api.post('/v1/session', { token: 'x' });
    expect(calls).toEqual(['/amail/v1/info', '/amail/v1/session']);
  });
});
