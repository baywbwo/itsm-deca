import { describe, expect, it } from 'vitest';
import { ScopedLoader } from './scopedLoader';

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}
const flush = () => new Promise((r) => setTimeout(r, 0));

describe('ScopedLoader', () => {
  it('ignores a late answer from the previous workspace and aborts it', async () => {
    const loader = new ScopedLoader<string, string>();
    const a = deferred<string>();
    const b = deferred<string>();
    let signalA: AbortSignal | undefined;
    loader.load('A', (s) => {
      signalA = s;
      return a.promise;
    });
    loader.load('B', () => b.promise);
    expect(signalA?.aborted).toBe(true);
    expect(loader.getState()).toEqual({ status: 'loading', key: 'B' });
    b.resolve('data-B');
    await flush();
    a.resolve('data-A');
    await flush();
    expect(loader.getState()).toEqual({ status: 'success', key: 'B', data: 'data-B' });
  });

  it('shows loading for the new key immediately, never the old data', async () => {
    const loader = new ScopedLoader<string, string>();
    loader.load('A', () => Promise.resolve('data-A'));
    await flush();
    expect(loader.getState()).toMatchObject({ status: 'success', key: 'A' });
    loader.load('B', () => new Promise(() => {}));
    expect(loader.getState()).toEqual({ status: 'loading', key: 'B' });
  });

  it('ignores a late error of an old key and abort errors', async () => {
    const loader = new ScopedLoader<string, string>();
    const a = deferred<string>();
    loader.load('A', () => a.promise);
    loader.load('B', () => Promise.reject(new DOMException('x', 'AbortError')));
    await flush();
    a.reject(new Error('late'));
    await flush();
    expect(loader.getState()).toEqual({ status: 'loading', key: 'B' });
  });

  it('reports errors for the current key and reset drops pending results', async () => {
    const loader = new ScopedLoader<string, string>();
    const err = new Error('boom');
    loader.load('A', () => Promise.reject(err));
    await flush();
    expect(loader.getState()).toEqual({ status: 'error', key: 'A', error: err });
    const c = deferred<string>();
    loader.load('C', () => c.promise);
    loader.reset();
    c.resolve('late');
    await flush();
    expect(loader.getState()).toEqual({ status: 'idle' });
  });
});
