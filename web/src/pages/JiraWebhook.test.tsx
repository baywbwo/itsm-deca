import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { createApi, type TransportRequest, type TransportResponse } from '../api/client';
import { isIntegrationList, isNotePage, isTicketIntegrations, isWebhookSecretResult } from '../api/guards';
import type { IntegrationConnection, Note } from '../api/types';
import { noteByline, webhookAddress } from '../app/integrations';
import { WebhookPanel } from './JiraWebhook';

const SECRET = 'ab'.repeat(32);
const path = '/api/v1/integrations/jira/i1/webhook';
const connection = {
  id: 'i1', provider: 'jira', displayName: 'DUMA Jira', mode: 'live', state: 'active', testedAt: '2026-11-12T01:00:00Z',
  lastTestNote: 'Terhubung', simulatedOutcome: 'success', version: 7, directoryCount: 2, lastTestOk: true,
  credential: { state: 'stored', updatedAt: '2026-11-12T01:00:00Z' },
  webhook: { state: 'missing', updatedAt: null, path, url: null },
  settings: { baseUrl: 'https://kantor.atlassian.net', email: 'svc@example.test', projectKey: 'OPS', issueTypeId: '10001', appId: null, priorityMap: {}, statusMap: { open: '10002' } },
  recent: [],
} as unknown as IntegrationConnection;
const LIST = { workspaceId: 'w1', creatable: [], canManageCredentials: true, integrationsMode: 'live', keyringConfigured: true, priorities: [], ticketStatuses: ['new'] };
const noop = () => undefined;
const panel = (over: Partial<Parameters<typeof WebhookPanel>[0]> = {}) => renderToStaticMarkup(
  <WebhookPanel connection={connection} timezone="Asia/Jakarta" origin="https://itsm.kantor.test" canManage keyring busy={false}
    error={null} revealed={null} onHide={noop} onRotate={noop} onDelete={noop} {...over} />,
);

describe('CP6a guards', () => {
  it('accept officer notes and Jira timeline entries, reject a mixed shape', () => {
    const note = { id: 'n1', kind: 'note', body: 'Catatan', author: { id: 'u1', label: 'Lead' }, externalAuthor: null, createdAt: '2026-11-12T01:00:00Z' };
    const jira = { id: 'n2', kind: 'jira_comment', body: 'Sudah dicek', author: null, externalAuthor: 'Dewi (Jira)', createdAt: '2026-11-12T02:00:00Z' };
    expect(isNotePage({ items: [note, jira], nextCursor: null })).toBe(true);
    expect(isNotePage({ items: [{ ...note, author: null }], nextCursor: null })).toBe(false);
    expect(isNotePage({ items: [{ ...jira, externalAuthor: null }], nextCursor: null })).toBe(false);
    expect(isNotePage({ items: [{ ...jira, author: { id: 'u1', label: 'Lead' } }], nextCursor: null })).toBe(false);
    expect(isNotePage({ items: [{ ...jira, kind: 'jira_worklog' }], nextCursor: null })).toBe(false);
  });

  it('a connection carries the webhook state but never a secret; the rotate answer has exactly one', () => {
    expect(isIntegrationList({ ...LIST, items: [connection, { ...connection, id: 'i2', provider: 'lark', webhook: null }] })).toBe(true);
    expect(isIntegrationList({ ...LIST, items: [{ ...connection, webhook: { state: 'configured', path } }] })).toBe(false);
    expect(isWebhookSecretResult({ item: connection, auditEventId: 'a1', secret: SECRET, path, url: null })).toBe(true);
    expect(isWebhookSecretResult({ item: connection, auditEventId: 'a1', secret: 'pendek', path, url: null })).toBe(false);
    expect(isWebhookSecretResult({ item: connection, auditEventId: 'a1', path, url: null })).toBe(false);
  });

  it('a status transition intent shows its target ticket status', () => {
    const sync = {
      id: 's1', provider: 'jira', operation: 'jira_transition_issue', state: 'failed', attempt: 1, targetLabel: null, issueKey: 'OPS-1',
      safeError: 'Workflow Jira tidak punya transisi ke status itu. Ubah manual di Jira.', reconciled: false, updatedAt: '2026-11-12T01:00:00Z',
      mode: 'live', nextAttemptAt: null, issueUrl: null, autoAttempts: 1, inFlight: false, targetStatus: 'pending_engineering',
    };
    expect(isTicketIntegrations({ ticketId: 't1', items: [sync], canRecover: true })).toBe(true);
    expect(isTicketIntegrations({ ticketId: 't1', items: [{ ...sync, targetStatus: 7 }], canRecover: true })).toBe(false);
  });
});

describe('CP6a helpers', () => {
  it('bylines name the Jira author and source; officer notes keep the ITSM name', () => {
    const base = { id: 'n', body: 'x', createdAt: '2026-11-12T01:00:00Z' };
    expect(noteByline({ ...base, kind: 'note', author: { id: 'u1', label: 'Lead Alfa' }, externalAuthor: null } as Note))
      .toEqual({ name: 'Lead Alfa', source: null });
    expect(noteByline({ ...base, kind: 'jira_status', author: null, externalAuthor: 'Rina' } as Note))
      .toEqual({ name: 'Rina', source: 'Status Jira' });
    expect(noteByline({ ...base, kind: 'jira_assignee', author: null, externalAuthor: null } as Note).name).toBe('Pengguna Jira');
  });

  it('the webhook address prefers the server public URL and marks a browser guess', () => {
    expect(webhookAddress({ path, url: 'https://itsm.example.test/x' }, 'https://lain')).toEqual({ url: 'https://itsm.example.test/x', guessed: false });
    expect(webhookAddress({ path, url: null }, 'https://itsm.kantor.test/')).toEqual({ url: `https://itsm.kantor.test${path}`, guessed: true });
  });
});

describe('CP6a webhook secret panel', () => {
  it('no secret yet: says deliveries are refused and offers to create one', () => {
    const html = panel();
    expect(html).toContain('Belum dibuat (semua kiriman ditolak)');
    expect(html).toContain(`https://itsm.kantor.test${path}`);
    expect(html).toContain('dari alamat browser ini');
    expect(html).toContain('Buat secret webhook');
    expect(html).not.toContain('Hapus secret webhook');
  });

  it('a stored secret is never rendered; only the one just generated is, with the warning', () => {
    const stored = { ...connection, webhook: { state: 'stored', updatedAt: '2026-11-12T03:00:00Z', path, url: 'https://itsm.example.test/hook' } } as IntegrationConnection;
    const html = panel({ connection: stored });
    expect(html).toContain('Ganti secret webhook');
    expect(html).toContain('Hapus secret webhook');
    expect(html).not.toContain('sekali tampil');
    const shown = panel({ connection: stored, revealed: { secret: SECRET, url: 'https://itsm.example.test/hook' } });
    expect(shown).toContain(`value="${SECRET}"`);
    expect(shown).toContain('tidak akan ditampilkan lagi');
    expect(shown).toMatch(/<label for="([^"]+)" class="tiny">Secret webhook \(sekali tampil\)<\/label><input id="\1"[^>]*readOnly/i);
  });

  it('without the credential grant or the server key, no action is offered', () => {
    expect(panel({ canManage: false })).toContain('tidak memegang hak kelola kredensial');
    expect(panel({ canManage: false })).not.toContain('<button');
    expect(panel({ keyring: false })).toMatch(/<button[^>]*disabled[^>]*title="Kunci enkripsi server belum dipasang"/);
  });

  it('Lark connections have no webhook section', () => {
    expect(panel({ connection: { ...connection, provider: 'lark', webhook: null } as IntegrationConnection })).toBe('');
  });
});

describe('CP6a api client', () => {
  it('rotate and delete hit the webhook-secret path with the version', async () => {
    const seen: TransportRequest[] = [];
    const answers: Record<string, TransportResponse> = {
      'POST /api/v1/workspaces/w1/integrations/i1/webhook-secret': { status: 200, requestId: null, body: { item: connection, auditEventId: 'a1', secret: SECRET, path, url: null } },
      'DELETE /api/v1/workspaces/w1/integrations/i1/webhook-secret': { status: 200, requestId: null, body: { item: connection, auditEventId: 'a2' } },
    };
    const api = createApi(async (req) => {
      if (req.path === '/api/v1/session/csrf') return { status: 200, body: { csrfToken: 'c' }, requestId: null };
      seen.push(req);
      return answers[`${req.method} ${req.path}`] ?? { status: 404, body: null, requestId: null };
    });
    expect((await api.rotateWebhookSecret('w1', 'i1', { version: 7 })).secret).toBe(SECRET);
    expect((await api.deleteWebhookSecret('w1', 'i1', { version: 8 })).auditEventId).toBe('a2');
    expect(seen.map((r) => [r.method, r.body])).toEqual([['POST', { version: 7 }], ['DELETE', { version: 8 }]]);
  });
});
