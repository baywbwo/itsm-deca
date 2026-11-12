import { describe, expect, it } from 'vitest';
import { planDrop } from './boardDnd';
import type { TicketAllowedAction, TicketListItem, TicketStatusCode } from '../api/types';

type Status = TicketListItem['status'];

const st = (code: TicketStatusCode, archived = false): Status => ({
  code, label: code, archived, selectable: !archived,
}) as Status;

const action = (to: TicketStatusCode, requires: TicketAllowedAction['requires'] = 'none', blockedBy: string | null = null): TicketAllowedAction =>
  ({ to: st(to), kind: requires === 'summary' ? 'resolve' : requires === 'reason' ? 'reopen' : 'move', requires, blockedBy }) as TicketAllowedAction;

const ticket = (status: TicketStatusCode, allowed: TicketAllowedAction[] = [], archived = false): TicketListItem => ({
  id: 't1', number: 12, displayNumber: 'DUMA-12', subject: 'Contoh', status: st(status, archived),
  cycleNo: 1, version: 3, priority: null, owner: null, channel: null, category: null,
  receivedAt: '2026-11-01T00:00:00Z', updatedAt: '2026-11-02T00:00:00Z', ageSeconds: 3600,
  relatedWork: { open: 0, blockingOpen: 0 }, allowedActions: allowed,
}) as unknown as TicketListItem;

describe('planDrop (board drag & drop)', () => {
  it('mengabaikan kartu yang dilepas di kolom asalnya', () => {
    expect(planDrop(ticket('open'), 'open')).toEqual({ kind: 'ignore' });
  });

  it('menolak tiket arsip tanpa melihat aksi yang tersedia', () => {
    const plan = planDrop(ticket('closed', [action('open')], true), 'open');
    expect(plan.kind).toBe('refuse');
    if (plan.kind === 'refuse') expect(plan.message).toContain('arsip');
  });

  it('menolak target yang tidak ada di allowedActions (server yang menentukan)', () => {
    const plan = planDrop(ticket('new', [action('open')]), 'resolved');
    expect(plan.kind).toBe('refuse');
    if (plan.kind === 'refuse') expect(plan.message).toContain('tidak dapat dipindah');
  });

  it('menampilkan alasan blokir dari server, bukan membuka dialog', () => {
    const plan = planDrop(ticket('open', [action('resolved', 'summary', 'Masih ada pekerjaan terkait yang menghambat.')]), 'resolved');
    expect(plan.kind).toBe('refuse');
    if (plan.kind === 'refuse') expect(plan.message).toContain('menghambat');
  });

  it('memindahkan status aktif lewat dialog move', () => {
    const plan = planDrop(ticket('new', [action('open')]), 'open');
    expect(plan.kind).toBe('dialog');
    if (plan.kind === 'dialog') {
      expect(plan.to).toBe('open');
      expect(plan.action.requires).toBe('none');
    }
  });

  it('resolve wajib lewat dialog karena butuh ringkasan', () => {
    const plan = planDrop(ticket('open', [action('resolved', 'summary')]), 'resolved');
    expect(plan.kind).toBe('dialog');
    if (plan.kind === 'dialog') expect(plan.action.requires).toBe('summary');
  });

  it('reopen dari terselesaikan wajib lewat dialog karena butuh alasan', () => {
    const plan = planDrop(ticket('resolved', [action('open', 'reason')]), 'open');
    expect(plan.kind).toBe('dialog');
    if (plan.kind === 'dialog') expect(plan.action.requires).toBe('reason');
  });

  it('menolak pindah dari terselesaikan ke status selain Ditangani (server tidak menawarkannya)', () => {
    const plan = planDrop(ticket('resolved', [action('open', 'reason')]), 'pending_engineering');
    expect(plan.kind).toBe('refuse');
  });
});
