// Loads data that belongs to one key (a workspace). Starting a new key aborts the previous
// request and ignores any late answer for it, so data of an old workspace can never be
// shown under a new one. Framework-free so it can be unit tested.

export type ScopedState<K, T> =
  | { status: 'idle' }
  | { status: 'loading'; key: K }
  | { status: 'success'; key: K; data: T }
  | { status: 'error'; key: K; error: unknown };

export class ScopedLoader<K, T> {
  private gen = 0;
  private controller: AbortController | null = null;
  private state: ScopedState<K, T> = { status: 'idle' };
  private listeners = new Set<() => void>();

  constructor(private readonly isAbort: (e: unknown) => boolean = defaultIsAbort) {}

  getState = (): ScopedState<K, T> => this.state;

  subscribe = (fn: () => void): (() => void) => {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  };

  load(key: K, fetcher: (signal: AbortSignal) => Promise<T>): void {
    this.controller?.abort();
    const controller = new AbortController();
    this.controller = controller;
    const gen = ++this.gen;
    this.set({ status: 'loading', key });
    fetcher(controller.signal).then(
      (data) => {
        if (gen === this.gen) this.set({ status: 'success', key, data });
      },
      (error: unknown) => {
        if (gen !== this.gen || this.isAbort(error)) return;
        this.set({ status: 'error', key, error });
      },
    );
  }

  /** Drop everything (e.g. session ended). */
  reset(): void {
    this.controller?.abort();
    this.controller = null;
    this.gen++;
    this.set({ status: 'idle' });
  }

  private set(s: ScopedState<K, T>) {
    this.state = s;
    for (const fn of this.listeners) fn();
  }
}

function defaultIsAbort(e: unknown): boolean {
  return typeof e === 'object' && e !== null && (e as { name?: unknown }).name === 'AbortError';
}
