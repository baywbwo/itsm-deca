import { describe, expect, it } from 'vitest';
import { ApiError, createApi } from '../api/client';
import fixtures from './fixtures.json';
import { replayTransport, type Fixtures, type Scenario } from './replayTransport';

const f = fixtures as Fixtures;
const ws = Object.keys(f.responses).find((k) => k.endsWith('/tickets#created'))!.split('/')[4]!;
const created = f.responses[`POST /api/v1/workspaces/${ws}/tickets#created`]!.body as { ticket: { id: string; displayNumber: string } };
const body = { subject: 'x', chronology: 'y', receivedAt: '2026-11-01T09:30:00+07:00', intakeChannelId: 'c' };

async function api(scenario: Scenario) {
  const a = createApi(replayTransport(f, 0, scenario));
  await a.enterUat();
  return a;
}

describe('D07 review replay (recorded server answers)', () => {
  it('same key + same payload replays the same ticket; another payload is the real 409', async () => {
    const a = await api('none');
    const first = await a.createTicket(ws, 'k1', body);
    expect(first.replayed).toBe(false);
    expect(first.ticket.id).toBe(created.ticket.id);
    const again = await a.createTicket(ws, 'k1', body);
    expect(again.replayed).toBe(true);
    expect(again.ticket.id).toBe(first.ticket.id);
    await expect(a.createTicket(ws, 'k1', { ...body, subject: 'z' })).rejects.toMatchObject({ status: 409, code: 'idempotency_conflict' });
  });

  it('a lost answer is unknown; the resend with the same key gets the existing ticket', async () => {
    const a = await api('intake-unknown');
    const err = await a.createTicket(ws, 'k1', body).catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ApiError);
    expect((err as ApiError).status).toBe(0);
    const resend = await a.createTicket(ws, 'k1', body);
    expect(resend.replayed).toBe(true);
    expect(resend.ticket.id).toBe(created.ticket.id);
  });

  it('degraded simulation keeps the ticket and reports jira unknown / lark failed honestly', async () => {
    const a = await api('intake-degraded');
    const r = await a.createTicket(ws, 'k1', body);
    expect(r.ticket.displayNumber).toMatch(/^[A-Z]+-\d+$/);
    const states = Object.fromEntries(r.integrations.map((i) => [i.provider, i.state]));
    expect(states).toEqual({ jira: 'unknown', lark: 'failed' });
    expect(r.integrations.every((i) => i.mode === 'simulation')).toBe(true);
  });

  it('client timeout surfaces as unknown (status 0, timeout)', async () => {
    const slow = createApi(async ({ signal }) => new Promise((_, reject) => {
      signal?.addEventListener('abort', () => reject(new DOMException('Aborted', 'AbortError')));
    }));
    await expect(slow.createTicket(ws, 'k1', body, 10)).rejects.toMatchObject({ status: 0, code: 'timeout' });
  });

  it('PIC change: recorded 200, and the real 409 in the conflict scenario', async () => {
    const path = Object.keys(f.responses).find((k) => k.endsWith('/assignment#ok'))!;
    const ticketId = path.split('/')[6]!;
    const ok = await (await api('none')).assignTicket(ws, ticketId, { ownerUserId: null, version: 1, reason: 'r' });
    expect(ok.ticket.id).toBe(ticketId);
    await expect((await api('conflict')).assignTicket(ws, ticketId, { ownerUserId: null, version: 1, reason: 'r' }))
      .rejects.toMatchObject({ status: 409 });
  });
});

describe('D08b review replay (recorded server answers)', () => {
  const rwKey = Object.keys(f.responses).find((k) => k.endsWith('/related-work#ok'))!;
  const tp = rwKey.split(' ')[1]!.replace(/\/related-work#ok$/, '');
  const [, , , , w, , ticketId] = tp.split('/') as string[];
  const completion = Object.keys(f.responses).find((k) => k.endsWith('/completion#ok'))!.split(' ')[1]!.replace('#ok', '');
  const workId = completion.split('/')[8]!;
  const rw = { title: 'Cek log settlement (dummy)', safeDescription: 'Tanpa data nasabah.', blocking: true };

  async function walk() {
    const a = await api('none');
    const d0 = await a.getTicketDetail(w!, ticketId!);
    await a.updateTicketProperties(w!, ticketId!, { version: d0.ticket.version });
    return a;
  }

  it('related work: same key + payload replays (200), another payload on the key is the real 409', async () => {
    const a = await walk();
    const first = await a.createRelatedWork(w!, ticketId!, 'k1', rw);
    expect(first.replayed).toBe(false);
    const again = await a.createRelatedWork(w!, ticketId!, 'k1', rw);
    expect(again).toMatchObject({ replayed: true, relatedWorkId: first.relatedWorkId, auditEventId: null });
    await expect(a.createRelatedWork(w!, ticketId!, 'k1', { ...rw, title: 'lain' })).rejects.toMatchObject({ status: 409, code: 'idempotency_conflict' });
  });

  it('note after the edit sequence: 201, the resend replays the same note, the list shows it', async () => {
    const a = await walk();
    await a.createRelatedWork(w!, ticketId!, 'k1', rw);
    await a.completeRelatedWork(w!, ticketId!, workId, { version: 1, summary: 's', evidence: 'e' });
    const text = { body: 'Merchant dihubungi; menunggu konfirmasi settlement (dummy, rekaman review D08b).' };
    const n = await a.createTicketNote(w!, ticketId!, 'n1', text);
    expect(n.replayed).toBe(false);
    expect((await a.createTicketNote(w!, ticketId!, 'n1', text))).toMatchObject({ replayed: true, note: { id: n.note.id } });
    const list = await a.listTicketNotes(w!, ticketId!, null);
    expect(list.items[0]?.id).toBe(n.note.id);
  });

  it('upload fails closed with the recorded scanner refusal; a disallowed type is the recorded 415', async () => {
    const a = await api('none');
    const png = new File([new Uint8Array([0x89, 0x50, 0x4e, 0x47])], 'bukti.png', { type: 'image/png' });
    await expect(a.uploadTicketAttachment(w!, ticketId!, png, 'internal')).rejects.toMatchObject({ status: 503, code: 'scanner_unavailable' });
    const txt = new File(['x'], 'catatan.txt', { type: 'text/plain' });
    await expect(a.uploadTicketAttachment(w!, ticketId!, txt, 'internal')).rejects.toMatchObject({ status: 415, code: 'attachment_type_refused' });
  });

  it('conversation panel is read-only: no adapter, nothing to send', async () => {
    const p = await (await api('none')).getTicketConversations(w!, ticketId!);
    expect(p.adapterConfigured).toBe(false);
  });
});
