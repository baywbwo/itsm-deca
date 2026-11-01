export type BadgeTone = 'neutral' | 'blue' | 'waiting' | 'ok' | 'error';

/** C07: the text label is always shown; colour only supports it. Not interactive. */
export function Badge({ tone = 'neutral', children }: { tone?: BadgeTone; children: string }) {
  return <span className={tone === 'neutral' ? 'badge' : `badge badge-${tone}`}>{children}</span>;
}
