import { describe, expect, it } from 'vitest';
import { defaultReportPeriod, parseReportQuery, reportExportBody, reportQuery } from '../app/routes';
import { createApi, type TransportRequest, type TransportResponse } from './client';
import { isReportPreview, isReportRun } from './guards';

const W = '/api/v1/workspaces/w1';
const status = (code: string, label: string) => ({ code, label, selectable: true, archived: false });
const preview = {
  reportType: 'operational', reportLabel: 'Rekap operasional (NON-REGULATOR)',
  reportTypes: [
    { code: 'operational', label: 'Rekap operasional', available: true, reason: null },
    { code: 'bi_regulator', label: 'Persiapan laporan BI', available: false, reason: 'Pemetaan regulator belum tersedia.' },
  ],
  header: {
    companyName: 'PT Dummy A', companyCode: 'DUMA', reporterCode: null, workspaceId: 'w1', workspaceCode: 'DUMA',
    timezone: 'Asia/Jakarta', periodStart: '2026-10-01', periodEnd: '2026-10-31', startAt: '2026-09-30T17:00:00Z',
    endAt: '2026-10-31T17:00:00Z', basis: 'received', basisLabel: 'Diterima pada periode',
  },
  asOf: '2026-11-02T01:00:00Z',
  filters: { status: null, owner: null, channel: null, category: null, priority: null },
  filterOptions: { owners: [], channels: [], categories: [], priorities: [] },
  totals: { tickets: 3, resolutionCycles: 2, resolvedTickets: 1, unassigned: 2, missingChannel: 0, missingCategory: 0, missingPriority: 0 },
  byStatus: [{ status: status('new', 'Baru'), count: 3 }],
  byPriority: [{ id: null, label: null, count: 3 }], byChannel: [], byCategory: [], byOwner: [],
  validation: [{ code: 'reporter_code_missing', level: 'warning', message: 'Kode pelapor belum dikonfigurasi.' }],
  rows: [{
    ticketId: 't1', displayNumber: 'DUMA-1', subject: '=HYPERLINK()', status: status('new', 'Baru'), priority: null,
    category: null, channel: null, owner: null, receivedLocal: '2026-10-01 00:00', resolutionCycles: 0, lastResolvedLocal: null,
  }],
  previewLimit: 50, exportMaxRows: 50000, exportable: true, notice: 'OPERASIONAL / NON-REGULATOR',
};
const run = {
  id: 'r1', reportType: 'operational', periodStart: '2026-10-01', periodEnd: '2026-10-31', state: 'succeeded',
  generatedAt: '2026-11-02T01:00:00Z', hasArtifact: true, createdAt: '2026-11-02T01:00:00Z', basis: 'received',
  filters: { status: null }, downloadable: true, requestedBy: { id: 'u1', label: 'Superadmin' }, rowCount: 3,
  artifactSha256: 'a'.repeat(64), artifactBytes: 512, expiresAt: '2026-11-03T01:00:00Z', expired: false,
  failureCode: null, fileName: 'DUMA-rekap-operasional-NON-REGULATOR-2026-10-01_sd_2026-10-31-received-r1.csv',
};

describe('D10 report query', () => {
  it('defaults to the current Jakarta month and keeps a canonical order', () => {
    // 2026-10-31T17:30Z is already 1 November in Jakarta.
    expect(defaultReportPeriod('Asia/Jakarta', Date.parse('2026-10-31T17:30:00Z')))
      .toEqual({ periodStart: '2026-11-01', periodEnd: '2026-11-01' });
    const p = parseReportQuery('status=closed,new,bogus,new&periodEnd=2026-10-31&periodStart=2026-10-01&owner=unassigned&basis=x', 'Asia/Jakarta');
    expect(p.status).toEqual(['new', 'closed']);
    expect(p.basis).toBe('received');
    expect(reportQuery(p)).toBe('periodStart=2026-10-01&periodEnd=2026-10-31&basis=received&status=new%2Cclosed&owner=unassigned');
    expect(reportQuery(parseReportQuery(reportQuery(p), 'Asia/Jakarta'))).toBe(reportQuery(p));
  });

  it('falls back on malformed dates and builds the export body of exactly that spec', () => {
    const p = parseReportQuery('periodStart=2026-1-1&periodEnd=abc&basis=resolved', 'Asia/Jakarta', Date.parse('2026-10-15T00:00:00Z'));
    expect(p).toMatchObject({ periodStart: '2026-10-01', periodEnd: '2026-10-15', basis: 'resolved', status: [] });
    expect(reportExportBody({ ...p, status: ['resolved'], filters: { owner: null, channel: 'none', category: null, priority: null } }))
      .toEqual({ reportType: 'operational', format: 'csv', periodStart: '2026-10-01', periodEnd: '2026-10-15', basis: 'resolved', status: ['resolved'], channel: 'none' });
  });
});

describe('D10 report guards and client', () => {
  it('accepts the contract shapes and rejects broken totals', () => {
    expect(isReportPreview(preview)).toBe(true);
    expect(isReportPreview({ ...preview, totals: { ...preview.totals, tickets: '3' } })).toBe(false);
    expect(isReportPreview({ ...preview, reportType: 'bi_regulator' })).toBe(false);
    expect(isReportRun(run)).toBe(true);
    expect(isReportRun({ ...run, state: 'done' })).toBe(false);
  });

  it('sends the idempotency key and CSRF token and accepts a 200 replay', async () => {
    const seen: TransportRequest[] = [];
    const responses: Record<string, TransportResponse> = {
      'POST /api/v1/uat/session': {
        status: 201, requestId: null,
        body: { user: { id: 'u', displayName: 'S' }, banner: 'b', uatSuperadmin: true, csrfToken: 'tok' },
      },
      [`POST ${W}/report-runs`]: { status: 200, requestId: null, body: { run, replayed: true } },
      [`GET ${W}/reports/preview?periodStart=2026-10-01&periodEnd=2026-10-31&basis=received`]: { status: 200, requestId: null, body: preview },
    };
    const api = createApi(async (req) => {
      seen.push(req);
      const r = responses[`${req.method} ${req.path}`];
      if (!r) throw new Error(`unexpected ${req.method} ${req.path}`);
      return r;
    });
    await api.enterUat();
    const pv = await api.previewReport('w1', 'periodStart=2026-10-01&periodEnd=2026-10-31&basis=received');
    expect(pv.totals.tickets).toBe(3);
    const body = reportExportBody(parseReportQuery('periodStart=2026-10-01&periodEnd=2026-10-31', 'Asia/Jakarta'));
    const r = await api.createReportRun('w1', 'k-1', body);
    expect(r.replayed).toBe(true);
    expect(seen[2]).toMatchObject({ method: 'POST', headers: { 'X-CSRF-Token': 'tok', 'Idempotency-Key': 'k-1' }, body });
    expect(api.reportContentUrl('w1', 'r1')).toBe(`${W}/report-runs/r1/content`);
  });

  it('maps the held BI export to a typed error', async () => {
    const api = createApi(async () => ({
      status: 422, requestId: 'r7',
      body: { error: { code: 'regulator_mapping_unavailable', message: 'Pemetaan regulator belum tersedia.' } },
    }));
    await expect(api.createReportRun('w1', 'k', { reportType: 'bi_regulator', periodStart: '2026-10-01', periodEnd: '2026-10-31' }))
      .rejects.toMatchObject({ status: 422, code: 'regulator_mapping_unavailable' });
  });
});
