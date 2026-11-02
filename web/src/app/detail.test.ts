import { describe, expect, it } from 'vitest';
import type { TicketProperties } from '../api/types';
import { isActivityPage } from '../api/guards';
import { diffProps, formFromProps, propertyDrafts, propsField, validateProps } from './detail';
import { parseDetailQuery, withDetail } from './routes';

const props: TicketProperties = {
  subject: 'Dana belum masuk',
  chronology: 'Kronologi dummy',
  receivedAt: '2026-11-01T02:00:00Z',
  intakeChannel: { id: 'ch1', label: 'Email' },
  priority: { id: 'p1', label: 'Tinggi' },
  category: null,
  product: null,
  receivingUnit: null,
  region: { id: 'r1', label: 'Jakarta' },
  merchant: null,
  transactionActivity: null,
  cause: 'Awal',
  foreignBranchReference: null,
};

describe('ticket detail properties', () => {
  it('sends nothing when nothing changed (whitespace only counts as unchanged)', () => {
    const f = formFromProps(props);
    expect(diffProps(f, props, 3)).toBeNull();
    expect(diffProps({ ...f, subject: '  Dana belum masuk ' }, props, 3)).toBeNull();
  });

  it('sends only changed fields with the base version; empty optional clears with null', () => {
    const f = { ...formFromProps(props), priorityId: '', categoryId: 'c2', cause: '  ', transactionActivity: ' Transfer ' };
    expect(diffProps(f, props, 7)).toEqual({ version: 7, priorityId: null, categoryId: 'c2', cause: null, transactionActivity: 'Transfer' });
  });

  it('validates required and length limits in characters', () => {
    const f = { ...formFromProps(props), subject: ' ', chronology: '', foreignBranchReference: 'é'.repeat(101) };
    expect(validateProps(f).map((e) => e.field)).toEqual(['subject', 'chronology', 'foreignBranchReference']);
    expect(validateProps({ ...f, subject: 'a', chronology: 'b', foreignBranchReference: 'é'.repeat(100) })).toEqual([]);
  });

  it('maps only known server fields to the form', () => {
    expect(propsField('regionId')).toBe('regionId');
    expect(propsField('version')).toBeNull();
    expect(propsField(null)).toBeNull();
  });

  it('keeps drafts in memory per workspace and ticket', () => {
    const d = { form: formFromProps(props), version: 2 };
    propertyDrafts.set('ws1', 't1', d);
    expect(propertyDrafts.get('ws1', 't1')).toBe(d);
    expect(propertyDrafts.get('ws2', 't1')).toBeUndefined();
    propertyDrafts.clear('ws1', 't1');
    expect(propertyDrafts.get('ws1', 't1')).toBeUndefined();
  });
});

describe('detail deep link', () => {
  it('adds and removes the ticket next to the list scope', () => {
    const q = withDetail('view=board&status=open', 't9');
    expect(parseDetailQuery(q)).toEqual({ ticketId: 't9', full: false });
    expect(new URLSearchParams(q).get('status')).toBe('open');
    expect(parseDetailQuery(withDetail(q, 't9', true))).toEqual({ ticketId: 't9', full: true });
    expect(withDetail(withDetail(q, 't9', true), null)).toBe('view=board&status=open');
  });

  it('ignores detail=full without a ticket', () => {
    expect(parseDetailQuery('detail=full')).toEqual({ ticketId: null, full: false });
  });
});

describe('activity guard', () => {
  it('accepts a page and rejects free text without a label', () => {
    expect(isActivityPage({ nextCursor: null, items: [{ id: 'a', at: 'x', action: 'ticket.updated', label: 'Mengubah penyebab', actor: null }] })).toBe(true);
    expect(isActivityPage({ nextCursor: null, items: [{ id: 'a', at: 'x', action: 'ticket.updated', actor: null }] })).toBe(false);
  });
});
