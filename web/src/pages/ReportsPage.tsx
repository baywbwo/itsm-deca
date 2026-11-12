import { useEffect, useRef, useState, type RefObject } from 'react';
import { ApiError, SESSION_RELOAD_TITLE, isSessionReloadNeeded, rateLimitText, sessionReloadText, isAbort, type Api } from '../api/client';
import type { ReportBreakdown, ReportFormat, ReportPreview, ReportRow, ReportRun, Workspace } from '../api/types';
import { instantLabel, num, rangeLabel, tzLabel } from '../app/format';
import { navigate } from '../app/hooks';
import {
  DASHBOARD_FILTERS,
  REPORT_STATUSES,
  defaultReportPeriod,
  parseReportQuery,
  reportExportBody,
  reportQuery,
  toHash,
  type DashboardFilterKey,
  type ReportParams,
  type ReportStatus,
} from '../app/routes';
import { Badge, type BadgeTone } from '../components/Badge';
import { Button } from '../components/Button';
import { DataTable, type Column } from '../components/DataTable';
import { PageHeader } from '../components/PageHeader';
import { EmptyState, ErrorState, ForbiddenState, LoadingState, NotFoundState } from '../components/States';
import { statusTone } from '../components/Badge';

const FORMAT_LABELS: Record<ReportFormat, string> = { csv: 'CSV', xlsx: 'Excel' };

const STATUS_LABELS: Record<ReportStatus, string> = {
  new: 'Baru', open: 'Ditangani', pending_customer: 'Menunggu pelanggan',
  pending_engineering: 'Menunggu pekerjaan eksternal', resolved: 'Terselesaikan', closed: 'Ditutup (arsip)',
};

const FILTERS: readonly { key: DashboardFilterKey; label: string; list: keyof ReportPreview['filterOptions']; unset: [string, string] }[] = [
  { key: 'owner', label: 'PIC', list: 'owners', unset: ['unassigned', 'Belum ditugaskan'] },
  { key: 'channel', label: 'Kanal', list: 'channels', unset: ['none', 'Tanpa kanal'] },
  { key: 'category', label: 'Kategori', list: 'categories', unset: ['none', 'Tanpa kategori'] },
  { key: 'priority', label: 'Prioritas', list: 'priorities', unset: ['none', 'Tanpa prioritas'] },
];

type Load<T> = { key: string; status: 'loading' } | { key: string; status: 'error'; error: unknown } | { key: string; status: 'success'; data: T };

/** Newest answer for `key` only: a switch aborts the old request and a late answer is dropped. */
function useLoad<T>(key: string, fetcher: (signal: AbortSignal) => Promise<T>, attempt: number): Load<T> {
  const [state, setState] = useState<Load<T>>({ key, status: 'loading' });
  const f = useRef(fetcher);
  f.current = fetcher;
  useEffect(() => {
    const ctl = new AbortController();
    setState({ key, status: 'loading' });
    f.current(ctl.signal).then(
      (data) => { if (!ctl.signal.aborted) setState({ key, status: 'success', data }); },
      (error: unknown) => { if (!ctl.signal.aborted && !isAbort(error)) setState({ key, status: 'error', error }); },
    );
    return () => ctl.abort();
  }, [key, attempt]);
  return state.key === key ? state : { key, status: 'loading' };
}

const newKey = () => (globalThis.crypto?.randomUUID?.() ?? `${Date.now()}-${Math.random().toString(16).slice(2)}`);

/**
 * C21/C22 operational report: company header from the server, Jakarta-time period, whole-scope
 * preview and a real CSV or Excel (.xlsx) export of exactly the previewed spec. The spec lives in the URL so reload
 * and shared links show the same numbers. Official BI export is held (no mapping is invented).
 */
export function ReportsPage({ api, workspace, query = '', onNotice }: {
  api: Api;
  workspace: Workspace;
  query?: string;
  onNotice: (text: string) => void;
}) {
  const params = parseReportQuery(query, workspace.timezone);
  const q = reportQuery(params);
  const [attempt, setAttempt] = useState(0);
  const [runsAttempt, setRunsAttempt] = useState(0);
  const preview = useLoad(`${workspace.id}|${q}`, (s) => api.previewReport(workspace.id, q, s), attempt);
  const runs = useLoad(workspace.id, (s) => api.listReportRuns(workspace.id, s), runsAttempt);
  const p = preview.status === 'success' ? preview.data : null;
  const options = useRef<ReportPreview['filterOptions'] | null>(null);
  if (p) options.current = p.filterOptions;

  // Draft dates: applied only when valid so a half-typed date never fires a request.
  const [start, setStart] = useState(params.periodStart);
  const [end, setEnd] = useState(params.periodEnd);
  useEffect(() => { setStart(params.periodStart); setEnd(params.periodEnd); }, [params.periodStart, params.periodEnd]);
  const dateError = !start || !end ? 'Isi tanggal awal dan akhir.' : start > end ? 'Tanggal awal harus sebelum atau sama dengan tanggal akhir.' : null;

  const go = (next: ReportParams) =>
    navigate(toHash({ workspaceId: workspace.id, page: 'reports', query: reportQuery(next) }));
  const applyPeriod = () => { if (!dateError) go({ ...params, periodStart: start, periodEnd: end }); };
  const toggleStatus = (c: ReportStatus) =>
    go({ ...params, status: params.status.includes(c) ? params.status.filter((x) => x !== c) : [...params.status, c] });
  const reset = () => {
    const d = defaultReportPeriod(workspace.timezone);
    go({ ...d, basis: 'received', status: [], filters: { owner: null, channel: null, category: null, priority: null } });
  };

  // Idempotency: a retry of the same spec after an unknown outcome reuses its key; a new spec or a
  // finished attempt gets a fresh key, so a later click is a new export rather than a silent replay.
  // The format is part of the spec: Excel and CSV of the same preview are separate runs.
  const pending = useRef<{ spec: string; key: string } | null>(null);
  const [exporting, setExporting] = useState<ReportFormat | null>(null);
  const [exportError, setExportError] = useState<{ title: string; body: string; requestId: string | null } | null>(null);
  const [lastRun, setLastRun] = useState<ReportRun | null>(null);
  const exportStatus = useRef<HTMLParagraphElement>(null);
  const spec = `${workspace.id}|${q}`;
  useEffect(() => { setExportError(null); setLastRun(null); }, [spec]);

  async function exportFile(format: ReportFormat) {
    const want = `${spec}|${format}`;
    if (pending.current?.spec !== want) pending.current = { spec: want, key: newKey() };
    const key = pending.current.key;
    setExporting(format);
    setExportError(null);
    try {
      const r = await api.createReportRun(workspace.id, key, reportExportBody(params, format));
      pending.current = null;
      setLastRun(r.run);
      setRunsAttempt((n) => n + 1);
      onNotice(r.run.state === 'succeeded'
        ? `Ekspor ${FORMAT_LABELS[r.run.format]} siap: ${num(r.run.rowCount ?? 0)} baris.`
        : 'Ekspor gagal; tidak ada file yang dibuat.');
    } catch (e) {
      // Only a definite rejection frees the key; network/5xx may have committed, so retry replays.
      if (e instanceof ApiError && e.status >= 400 && e.status < 500) pending.current = null;
      setExportError(exportErrorText(e));
    } finally {
      setExporting(null);
      exportStatus.current?.focus();
    }
  }

  const tz = p?.header.timezone ?? workspace.timezone;
  const filtered = params.status.length > 0 || DASHBOARD_FILTERS.some((k) => params.filters[k]);
  const bi = p?.reportTypes.find((t) => t.code === 'bi_regulator');

  return (
    <>
      <PageHeader eyebrow="Laporan operasional" title="Laporan"
        description={`Rekap pengaduan ${workspace.name} untuk keperluan operasional internal. Bukan laporan regulator.`} />
      <p className="banner spaced-bottom" role="note">
        <strong>Ekspor resmi BI belum tersedia.</strong>{' '}
        {bi?.reason ?? 'Pemetaan format regulator belum ditetapkan, jadi tidak ada kode BI yang dibuat atau ditebak.'}{' '}
        Semua angka dan file di halaman ini berlabel OPERASIONAL / NON-REGULATOR.
      </p>

      <section className="card" aria-labelledby="rep-company">
        <h2 id="rep-company">Perusahaan pelapor</h2>
        {p ? (
          <dl className="report-head">
            <div><dt>Perusahaan</dt><dd>{p.header.companyName} ({p.header.companyCode})</dd></div>
            <div><dt>Kode pelapor</dt><dd>{p.header.reporterCode ?? 'belum dikonfigurasi'}</dd></div>
            <div><dt>Workspace</dt><dd>{p.header.workspaceCode}</dd></div>
            <div><dt>Periode</dt><dd>{rangeLabel(p.header.periodStart, p.header.periodEnd)} · Waktu {tzLabel(tz)}</dd></div>
          </dl>
        ) : <p className="muted">Perusahaan dibaca dari server untuk workspace {workspace.code}; tidak dapat diubah di sini.</p>}
      </section>

      <form className="toolbar row filterbar report-filters" aria-label="Parameter laporan"
        onSubmit={(e) => { e.preventDefault(); applyPeriod(); }}>
        <label className="period">
          Jenis laporan
          <select className="field-control" value="operational" aria-describedby="rep-type-hint" onChange={() => undefined}>
            <option value="operational">Rekap operasional</option>
            <option value="bi_regulator" disabled>Persiapan laporan BI (belum tersedia)</option>
          </select>
        </label>
        <label className="period">
          Tanggal awal
          <input type="date" className="field-control" value={start} max={end || undefined}
            aria-invalid={dateError ? true : undefined} aria-describedby={dateError ? 'rep-date-error' : undefined}
            onChange={(e) => setStart(e.target.value)} onBlur={applyPeriod} />
        </label>
        <label className="period">
          Tanggal akhir
          <input type="date" className="field-control" value={end} min={start || undefined}
            aria-invalid={dateError ? true : undefined} aria-describedby={dateError ? 'rep-date-error' : undefined}
            onChange={(e) => setEnd(e.target.value)} onBlur={applyPeriod} />
        </label>
        <label className="period">
          Dasar tanggal
          <select className="field-control" value={params.basis}
            onChange={(e) => go({ ...params, basis: e.target.value === 'resolved' ? 'resolved' : 'received' })}>
            <option value="received">Tanggal diterima</option>
            <option value="resolved">Tanggal diselesaikan</option>
          </select>
        </label>
        {FILTERS.map((f) => {
          const value = params.filters[f.key] ?? '';
          const list = options.current?.[f.list] ?? [];
          const known = value === '' || value === f.unset[0] || list.some((o) => o.id === value);
          return (
            <label className="period" key={f.key}>
              {f.label}
              <select className="field-control" value={value} disabled={!options.current && value === ''}
                onChange={(e) => go({ ...params, filters: { ...params.filters, [f.key]: e.target.value || null } })}>
                <option value="">Semua</option>
                {list.map((o) => <option key={o.id} value={o.id}>{o.label}{o.active ? '' : ' (nonaktif)'}</option>)}
                <option value={f.unset[0]}>{f.unset[1]}</option>
                {!known && <option value={value}>Nilai tidak dikenal</option>}
              </select>
            </label>
          );
        })}
        <button type="submit" className="sr-only">Terapkan periode</button>
        {(filtered || params.basis !== 'received') && <Button variant="ghost" onClick={reset}>Reset</Button>}
        <fieldset className="report-status">
          <legend>Status (kosong = semua)</legend>
          {REPORT_STATUSES.map((c) => (
            <label key={c} className="check">
              <input type="checkbox" checked={params.status.includes(c)} onChange={() => toggleStatus(c)} />
              {p?.byStatus.find((s) => s.status.code === c)?.status.label ?? STATUS_LABELS[c]}
            </label>
          ))}
        </fieldset>
        <p id="rep-type-hint" className="muted tiny full">
          Persiapan laporan BI dinonaktifkan: {(bi?.reason ?? 'pemetaan regulator belum tersedia').replace(/\.$/, '')}.
        </p>
        {dateError && <p id="rep-date-error" className="field-error full" role="alert">{dateError}</p>}
      </form>

      <p className="sr-only" role="status">{p ? `Pratinjau laporan ${rangeLabel(p.header.periodStart, p.header.periodEnd)} dimuat.` : ''}</p>
      {preview.status === 'loading' && <LoadingState label="Memuat pratinjau laporan…" />}
      {preview.status === 'error' && (
        <PreviewError error={preview.error} onRetry={() => setAttempt((n) => n + 1)} onReset={reset} />
      )}
      {p && (
        <PreviewBody p={p} filtered={filtered} exporting={exporting} onExport={exportFile}
          exportError={exportError} lastRun={lastRun} statusRef={exportStatus} contentUrl={(id) => api.reportContentUrl(workspace.id, id)} />
      )}

      <section className="card spaced" aria-labelledby="rep-runs">
        <div className="row between">
          <h2 id="rep-runs">Riwayat ekspor</h2>
          <Button variant="ghost" onClick={() => setRunsAttempt((n) => n + 1)}>Muat ulang</Button>
        </div>
        <p className="tiny muted">Ekspor terbaru di workspace ini · file tersedia 24 jam · setiap unduhan dicek ulang izin, masa berlaku dan hash-nya.</p>
        {runs.status === 'loading' && <LoadingState label="Memuat riwayat ekspor…" rows={2} />}
        {runs.status === 'error' && (
          <ErrorState title="Riwayat ekspor belum dapat dimuat" body={exportErrorText(runs.error).body}
            requestId={runs.error instanceof ApiError ? runs.error.requestId : null} onRetry={() => setRunsAttempt((n) => n + 1)} />
        )}
        {runs.status === 'success' && (
          <DataTable caption="Riwayat ekspor laporan" columns={runColumns(tz, (id) => api.reportContentUrl(workspace.id, id))}
            rows={runs.data.items} rowKey={(r) => r.id}
            empty={{ title: 'Belum ada ekspor', body: 'Ekspor operasional Excel / CSV yang dibuat di workspace ini muncul di sini.' }} />
        )}
      </section>
    </>
  );
}

function exportErrorText(e: unknown): { title: string; body: string; requestId: string | null } {
  const requestId = e instanceof ApiError ? e.requestId : null;
  if (e instanceof ApiError) {
    if (e.code === 'regulator_mapping_unavailable') return { title: 'Ekspor BI belum tersedia', body: e.message, requestId };
    if (isSessionReloadNeeded(e)) return { title: SESSION_RELOAD_TITLE, body: sessionReloadText(null), requestId };
    if (e.status === 429) return { title: 'Terlalu banyak permintaan', body: `${rateLimitText(e)} Ekspor belum dibuat; filter Anda tetap.`, requestId };
    if (e.status === 409) return { title: 'Permintaan ekspor bertabrakan', body: 'Kunci permintaan sudah dipakai untuk parameter lain. Klik ekspor lagi.', requestId };
    if (e.status === 403) return { title: 'Akses ditolak', body: 'Anda tidak memiliki izin ekspor di workspace ini.', requestId };
    if (e.status === 422) return { title: 'Parameter tidak valid', body: e.message, requestId };
    if (e.code === 'network') return { title: 'Server tidak dapat dihubungi', body: 'Hasil ekspor belum diketahui. Coba lagi; permintaan yang sama tidak membuat file ganda.', requestId };
    if (e.code === 'dependency_unavailable') return { title: 'Ekspor gagal', body: `${e.message} Hasil ekspor belum diketahui; coba lagi tanpa membuat file ganda.`, requestId };
  }
  return { title: 'Ekspor gagal', body: 'Hasil ekspor belum diketahui. Coba lagi; permintaan yang sama tidak membuat file ganda.', requestId };
}

function PreviewError({ error, onRetry, onReset }: { error: unknown; onRetry: () => void; onReset: () => void }) {
  if (error instanceof ApiError && error.status === 403) return <ForbiddenState />;
  if (error instanceof ApiError && error.status === 404) return <NotFoundState />;
  if (error instanceof ApiError && (error.status === 422 || error.status === 400)) {
    return <ErrorState title="Parameter laporan tidak valid" body={`${error.message} Tidak ada angka yang ditampilkan.`}
      requestId={error.requestId} onRetry={onReset} retryLabel="Reset parameter" />;
  }
  const body = error instanceof ApiError && error.code === 'network'
    ? 'Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.'
    : error instanceof ApiError && error.code === 'dependency_unavailable'
      ? `${error.message} Tidak ada angka yang ditampilkan.`
      : 'Pratinjau tidak ditampilkan karena permintaan gagal. Tidak ada angka lama yang dipakai.';
  return <ErrorState title="Pratinjau laporan belum dapat dimuat" body={body}
    requestId={error instanceof ApiError ? error.requestId : null} onRetry={onRetry} />;
}

const LEVEL_TONE: Record<string, BadgeTone> = { info: 'blue', warning: 'waiting', blocking: 'error' };
const LEVEL_LABEL: Record<string, string> = { info: 'Info', warning: 'Perlu dicek', blocking: 'Menghambat' };

function PreviewBody({ p, filtered, exporting, onExport, exportError, lastRun, statusRef, contentUrl }: {
  p: ReportPreview;
  filtered: boolean;
  exporting: ReportFormat | null;
  onExport: (format: ReportFormat) => void;
  exportError: { title: string; body: string; requestId: string | null } | null;
  lastRun: ReportRun | null;
  statusRef: RefObject<HTMLParagraphElement | null>;
  contentUrl: (runId: string) => string;
}) {
  const t = p.totals;
  const tz = p.header.timezone;
  const basisText = p.header.basis === 'resolved' ? 'diselesaikan pada periode' : 'diterima pada periode';
  return (
    <>
      <div className="kpis">
        <div className="kpi kpi-primary"><span className="muted">Tiket unik</span><strong>{num(t.tickets)}</strong><small>{p.header.basisLabel} · = baris ekspor</small></div>
        <div className="kpi"><span className="muted">Siklus penyelesaian</span><strong>{num(t.resolutionCycles)}</strong><small>Peristiwa selesai dalam periode · bukan jumlah tiket</small></div>
        <div className="kpi"><span className="muted">Tiket terselesaikan</span><strong>{num(t.resolvedTickets)}</strong><small>Tiket unik dengan ≥1 penyelesaian dalam periode</small></div>
        <div className="kpi"><span className="muted">Belum ditugaskan</span><strong>{num(t.unassigned)}</strong><small>Tanpa PIC saat ini</small></div>
      </div>

      {p.validation.length > 0 && (
        <section className="card" aria-labelledby="rep-validation">
          <h2 id="rep-validation">Validasi sebelum ekspor</h2>
          <ul className="plain">
            {p.validation.map((v) => (
              <li className="row activity" key={v.code}>
                <Badge tone={LEVEL_TONE[v.level] ?? 'neutral'}>{LEVEL_LABEL[v.level] ?? v.level}</Badge>
                <span>{v.message}</span>
              </li>
            ))}
          </ul>
        </section>
      )}

      <section className="card spaced" aria-labelledby="rep-export">
        <h2 id="rep-export">Ekspor</h2>
        <p className="tiny muted">
          File memuat seluruh {num(t.tickets)} tiket sesuai parameter di atas (bukan hanya pratinjau), berlabel OPERASIONAL / NON-REGULATOR.
          Excel (.xlsx): sheet Data dengan tanggal dan angka bertipe, sheet Ringkasan berisi filter dan total; teks tidak pernah dijalankan
          sebagai rumus. CSV: sel diawali =, +, -, @ dinetralkan. Maksimal {num(p.exportMaxRows)} baris; melebihi batas = gagal, tidak dipotong.
        </p>
        <div className="row">
          <Button variant="primary" onClick={() => onExport('xlsx')} disabled={exporting !== null || !p.exportable} aria-describedby="rep-export-state">
            {exporting === 'xlsx' ? 'Menyiapkan Excel…' : 'Ekspor Excel (.xlsx)'}
          </Button>
          <Button onClick={() => onExport('csv')} disabled={exporting !== null || !p.exportable} aria-describedby="rep-export-state">
            {exporting === 'csv' ? 'Menyiapkan CSV…' : 'Ekspor CSV'}
          </Button>
          <Button disabled title="Pemetaan regulator belum tersedia">Kirim ke BI (belum tersedia)</Button>
        </div>
        <p id="rep-export-state" ref={statusRef} tabIndex={-1} className="tiny" role="status" aria-live="polite">
          {exporting ? 'Status: membangun file…'
            : !p.exportable ? 'Ekspor tidak tersedia: perbaiki item validasi yang menghambat.'
              : lastRun ? runSummary(lastRun, tz) : ''}
          {!exporting && lastRun?.downloadable && <> · <a href={contentUrl(lastRun.id)} download={lastRun.fileName ?? undefined}>Unduh {lastRun.fileName}</a></>}
        </p>
        {exportError && <ErrorState title={exportError.title} body={exportError.body} requestId={exportError.requestId} />}
      </section>

      {t.tickets === 0 ? (
        <EmptyState title="Tidak ada tiket pada periode ini"
          body={filtered ? 'Tidak ada tiket yang cocok dengan kombinasi filter dan periode ini.' : `Tidak ada tiket ${basisText} di ${p.header.workspaceCode}.`} />
      ) : (
        <>
          <section className="card spaced" aria-labelledby="rep-status">
            <h2 id="rep-status">Rekap berdasarkan status</h2>
            <p className="tiny muted">Status saat ini dari {num(t.tickets)} tiket unik pada laporan</p>
            <div className="tablebox" role="region" aria-label="Rekap berdasarkan status" tabIndex={0}>
              <table>
                <caption className="sr-only">Rekap berdasarkan status</caption>
                <thead><tr><th scope="col">Status</th><th scope="col">Tiket</th></tr></thead>
                <tbody>
                  {p.byStatus.map((s) => (
                    <tr key={s.status.code}><td><Badge tone={statusTone(s.status)}>{s.status.label}</Badge></td><td>{num(s.count)}</td></tr>
                  ))}
                </tbody>
                <tfoot><tr><th scope="row">Total</th><td>{num(t.tickets)}</td></tr></tfoot>
              </table>
            </div>
          </section>
          <div className="grid2 spaced report-breakdowns">
            <Breakdown id="rep-priority" title="Per prioritas" rows={p.byPriority} none="Tanpa prioritas" />
            <Breakdown id="rep-channel" title="Per kanal" rows={p.byChannel} none="Tanpa kanal" />
            <Breakdown id="rep-category" title="Per kategori" rows={p.byCategory} none="Tanpa kategori" />
            <Breakdown id="rep-owner" title="Per PIC" rows={p.byOwner} none="Belum ditugaskan" />
          </div>
          <section className="card spaced" aria-labelledby="rep-rows">
            <h2 id="rep-rows">Pratinjau baris</h2>
            <p className="tiny muted">
              {num(p.rows.length)} dari {num(t.tickets)} baris pertama dalam urutan file (diterima, lalu nomor). Total di atas dihitung
              server dari seluruh baris, bukan dari halaman ini.
            </p>
            <DataTable caption="Pratinjau baris laporan" columns={ROW_COLUMNS} rows={p.rows} rowKey={(r) => r.ticketId}
              empty={{ title: 'Tidak ada baris', body: 'Tidak ada tiket pada laporan ini.' }} />
          </section>
        </>
      )}
      <p className="tiny muted source">
        {p.notice} · per {instantLabel(p.asOf, tz)} {tzLabel(tz)} · rentang UTC {p.header.startAt} s.d. sebelum {p.header.endAt}
      </p>
    </>
  );
}

function Breakdown({ id, title, rows, none }: { id: string; title: string; rows: readonly ReportBreakdown[]; none: string }) {
  return (
    <section className="card" aria-labelledby={id}>
      <h2 id={id}>{title}</h2>
      <ul className="plain">
        {rows.map((r) => (
          <li className="row between activity" key={r.id ?? 'none'}><span>{r.label ?? none}</span><b>{num(r.count)}</b></li>
        ))}
      </ul>
    </section>
  );
}

const ROW_COLUMNS: readonly Column<ReportRow>[] = [
  { key: 'ticket', header: 'Nomor / tiket', cell: (r) => <><b>{r.displayNumber}</b><span className="subject muted" title={r.subject}>{r.subject}</span></> },
  { key: 'status', header: 'Status', cell: (r) => <Badge tone={statusTone(r.status)}>{r.status.label}</Badge> },
  { key: 'priority', header: 'Prioritas', cell: (r) => r.priority ?? '—' },
  { key: 'owner', header: 'PIC', cell: (r) => r.owner ?? 'Belum ditugaskan' },
  { key: 'channel', header: 'Kanal', cell: (r) => r.channel ?? '—' },
  { key: 'received', header: 'Diterima', cell: (r) => r.receivedLocal },
  { key: 'cycles', header: 'Siklus selesai', cell: (r) => num(r.resolutionCycles) },
  { key: 'resolved', header: 'Selesai terakhir', cell: (r) => r.lastResolvedLocal ?? '—' },
];

function runState(r: ReportRun): { label: string; tone: BadgeTone } {
  if (r.state === 'failed') return { label: r.failureCode === 'row_limit' ? 'Gagal · melebihi batas baris' : 'Gagal', tone: 'error' };
  if (r.state === 'queued') return { label: 'Antre', tone: 'neutral' };
  if (r.state === 'running') return { label: 'Dibangun', tone: 'blue' };
  if (r.expired) return { label: 'Kedaluwarsa', tone: 'neutral' };
  return r.downloadable ? { label: 'Siap', tone: 'ok' } : { label: 'Selesai · tanpa file', tone: 'neutral' };
}

function runSummary(r: ReportRun, tz: string): string {
  const s = runState(r).label;
  if (r.state === 'failed') return `Status: ${s}. Tidak ada file yang dibuat.`;
  return `Status: ${s} · ${num(r.rowCount ?? 0)} baris${r.expiresAt ? ` · berlaku s.d. ${instantLabel(r.expiresAt, tz)}` : ''}`;
}

function runColumns(tz: string, url: (id: string) => string): readonly Column<ReportRun>[] {
  return [
    { key: 'created', header: 'Dibuat', cell: (r) => `${instantLabel(r.createdAt, tz)} · ${r.requestedBy.label}` },
    { key: 'period', header: 'Periode', cell: (r) => `${rangeLabel(r.periodStart, r.periodEnd)} · ${r.basis === 'resolved' ? 'diselesaikan' : 'diterima'}` },
    { key: 'state', header: 'Status', cell: (r) => { const s = runState(r); return <Badge tone={s.tone}>{s.label}</Badge>; } },
    { key: 'format', header: 'Format', cell: (r) => FORMAT_LABELS[r.format] },
    { key: 'rows', header: 'Baris', cell: (r) => (r.rowCount === null ? '—' : num(r.rowCount)) },
    {
      key: 'sha', header: 'SHA-256',
      cell: (r) => (r.artifactSha256 ? <code title={r.artifactSha256}>{r.artifactSha256.slice(0, 12)}…</code> : '—'),
    },
    { key: 'expires', header: 'Berlaku s.d.', cell: (r) => (r.expiresAt ? instantLabel(r.expiresAt, tz) : '—') },
    {
      key: 'download', header: 'File',
      cell: (r) => (r.downloadable
        ? <a href={url(r.id)} download={r.fileName ?? undefined} aria-label={`Unduh ${r.fileName ?? FORMAT_LABELS[r.format]}`}>Unduh {FORMAT_LABELS[r.format]}</a>
        : <span className="muted">{r.expired ? 'Kedaluwarsa' : 'Tidak tersedia'}</span>),
    },
  ];
}
