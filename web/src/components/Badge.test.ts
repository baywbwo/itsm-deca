import { describe, expect, it } from 'vitest';
import { statusTone } from './Badge';

// CP-UI1 status chips (owner prototype): Baru/Ditangani blue, Menunggu pelanggan amber,
// Menunggu eksternal violet, Terselesaikan green; unknown codes stay neutral.
describe('statusTone', () => {
  it('maps every workflow status to its prototype chip tone', () => {
    expect((['new', 'open', 'pending_customer', 'pending_engineering', 'resolved', 'closed'] as const)
      .map((code) => statusTone({ code }))).toEqual(['blue', 'open', 'waiting', 'violet', 'ok', 'neutral']);
  });
});
