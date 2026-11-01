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
