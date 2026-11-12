import type { TicketStatus } from '../api/types';

/** Prototype chip colours (reference-owner-v2 §badges); `open` is sapphire, `violet` waits on another team. */
export type BadgeTone = 'neutral' | 'blue' | 'open' | 'waiting' | 'violet' | 'ok' | 'error';

/** C07: the text label is always shown; colour only supports it. Not interactive. */
export function Badge({ tone = 'neutral', children }: { tone?: BadgeTone; children: string }) {
  return <span className={tone === 'neutral' ? 'badge' : `badge badge-${tone}`}>{children}</span>;
}

/**
 * Ticket status chip colour (owner prototype): Baru blue, Ditangani sapphire, Menunggu pelanggan
 * amber, Menunggu eksternal violet, Terselesaikan green, Ditutup grey. Unknown codes stay grey.
 */
export function statusTone(s: Pick<TicketStatus, 'code'>): BadgeTone {
  switch (s.code) {
    case 'new': return 'blue';
    case 'open': return 'open';
    case 'pending_customer': return 'waiting';
    case 'pending_engineering': return 'violet';
    case 'resolved': return 'ok';
    default: return 'neutral';
  }
}
