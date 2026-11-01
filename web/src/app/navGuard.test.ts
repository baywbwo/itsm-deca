import { beforeAll, describe, expect, it } from 'vitest';

// Minimal browser stand-in: hash URL, history entries and hashchange events (node test env).
type Listener = () => void;
const hl = new Set<Listener>();
const loc = { hash: '', pathname: '/', search: '' };
const entries: string[] = [''];
let at = 0;
const fire = () => hl.forEach((fn) => fn());
const fakeWindow = {
  location: {
    get hash() { return loc.hash; },
    set hash(v: string) { entries.splice(at + 1); entries.push(v); at += 1; loc.hash = v; fire(); },
    get pathname() { return loc.pathname; },
    get search() { return loc.search; },
  },
  history: {
    pushState: (_: unknown, __: string, url: string) => { const h = url.slice(url.indexOf('#')); entries.splice(at + 1); entries.push(h); at += 1; loc.hash = h; },
    replaceState: (_: unknown, __: string, url: string) => { const h = url.slice(url.indexOf('#')); entries[at] = h; loc.hash = h; },
  },
  addEventListener: (t: string, fn: Listener) => { if (t === 'hashchange') hl.add(fn); },
  removeEventListener: (t: string, fn: Listener) => { if (t === 'hashchange') hl.delete(fn); },
};
const back = () => { at -= 1; loc.hash = entries[at] ?? ""; fire(); };

let h: typeof import('./hooks');
let r: typeof import('./routes');
let dirty = true;

beforeAll(async () => {
  (globalThis as unknown as { window: unknown }).window = fakeWindow;
  loc.hash = entries[0] = '#/w/W1/tickets';
  h = await import('./hooks');
  r = await import('./routes');
  h.subscribeRoute(() => undefined);
  h.setNavGuard((from, to) => r.leavesScope(from, to) && dirty);
});

describe('D07r draft leave guard', () => {
  it('only page or workspace changes count as leaving; filters never ask', () => {
    const a = r.parseHash('#/w/W1/tickets?status=open');
    expect(r.leavesScope(a, r.parseHash('#/w/W1/tickets?status=closed&sort=id'))).toBe(false);
    expect(r.leavesScope(a, r.parseHash('#/w/W1/dashboard'))).toBe(true);
    expect(r.leavesScope(a, r.parseHash('#/w/W2/tickets?status=open'))).toBe(true);
  });

  it('a query-only navigate passes even with a dirty draft', () => {
    expect(h.navigate('#/w/W1/tickets?status=open')).toBe(true);
    expect(h.currentHash()).toBe('#/w/W1/tickets?status=open');
  });

  it('a nav link to another page is held; Stay keeps the route and the URL', () => {
    expect(h.navigate('#/w/W1/dashboard')).toBe(false);
    expect(h.currentHash()).toBe('#/w/W1/tickets?status=open');
    expect(loc.hash).toBe('#/w/W1/tickets?status=open');
    h.resolvePendingNav(false);
    expect(h.currentHash()).toBe('#/w/W1/tickets?status=open');
  });

  it('Continue goes to the held target', () => {
    expect(h.navigate('#/w/W1/dashboard')).toBe(false);
    h.resolvePendingNav(true);
    expect(h.currentHash()).toBe('#/w/W1/dashboard');
    expect(loc.hash).toBe('#/w/W1/dashboard');
  });

  it('Back with a dirty draft is undone and asked; Continue then leaves', () => {
    back(); // dashboard → tickets?status=open: page change
    expect(h.currentHash()).toBe('#/w/W1/dashboard');
    expect(loc.hash).toBe('#/w/W1/dashboard');
    h.resolvePendingNav(true);
    expect(h.currentHash()).toBe('#/w/W1/tickets?status=open');
  });

  it('nothing is asked when the draft is clean', () => {
    dirty = false;
    expect(h.navigate('#/w/W2/dashboard')).toBe(true);
    back();
    expect(h.currentHash()).toBe('#/w/W1/tickets?status=open');
    dirty = true;
  });

  it('force skips the guard (the user already decided)', () => {
    expect(h.navigate('#/w/W2/tickets', false, true)).toBe(true);
    expect(h.currentHash()).toBe('#/w/W2/tickets');
  });
});
