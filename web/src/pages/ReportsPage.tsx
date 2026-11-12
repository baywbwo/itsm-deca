import { useEffect, useRef, useState } from 'react';
import { ApiError, SESSION_RELOAD_TITLE, isSessionReloadNeeded, rateLimitText, sessionReloadText, isAbort, type Api } from '../api/client';
import type { ReportBreakdown, ReportFormat, ReportPreview, ReportRow, ReportRun, Workspace } from '../api/types';
import { instantLabel, num, rangeLabel, tzLabel } from '../app/format';
import { navigate } from '../app/hooks';
import {
  DASHBOARD_FILTERS,
  REPORT_PRESETS,
  REPORT_STATUSES,
  defaultReportPeriod,
  reportPresetOf,
  reportPresetPeriod,
  parseReportQuery,
  reportExportBody,
  reportQuery,
  toHash,
  type DashboardFilterKey,
  type ReportParams,
  type ReportPreset,
  type ReportStatus,
} from '../app/routes';
import { Badge, type BadgeTone } from '../components/Badge';
import { Button } from '../components/Button';
import { DataTable, type Column } from '../components/DataTable';
import { PageHeader } from '../components/PageHeader';
import { Icon } from '../components/Icon';
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
 * CP-UI3b (prototype 07): period presets (30 days default); the preview loads in the background (it
 * feeds the filter choices and the export check) but its numbers show only after "Pratinjau".
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
  // "Rentang kustom" keeps the date inputs open even when the typed dates happen to equal a preset.
  const [custom, setCustom] = useState(false);
  const preset = custom ? 'custom' : reportPresetOf(params, workspace.timezone);
  // Numbers appear only after the person asks for the preview (prototype 07); a new workspace starts empty.
  const [shown, setShown] = useState(false);
  useEffect(() => { setShown(false); setCustom(false); }, [workspace.id]);

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
  const morePicked = params.status.length > 0 || !!params.filters.owner || !!params.filters.priority;
  const refresh = () => {
    if (dateError) return;
    const wasShown = shown;
    setShown(true);
    if (start !== params.periodStart || end !== params.periodEnd) applyPeriod();
    // The first "Pratinjau" reveals the preview already loaded in the background; later clicks reload it.
    else if (wasShown || preview.status !== 'success') setAttempt((n) => n + 1);
  };
  const pickPreset = (v: string) => {
    if (v === 'custom') { setCustom(true); return; }
    setCustom(false);
    go({ ...params, ...reportPresetPeriod(v as ReportPreset, workspace.timezone) });
  };
  const blocking = p ? p.validation.filter((v) => v.level === 'blocking').length : 0;
  const exportBlocked = exporting !== null || !p || !p.exportable || !!dateError;

  const filterSelect = (f: (typeof FILTERS)[number]) => {
    const value = params.filters[f.key] ?? '';
    const list = options.current?.[f.list] ?? [];
    const known = value === '' || value === f.unset[0] || list.some((o) => o.id === value);
    return (
      <label className="field" key={f.key}>
        {f.label}
        <select className="field-control" value={value} disabled={!options.current && value === ''}
          onChange={(e) => go({ ...params, filters: { ...params.filters, [f.key]: e.target.value || null } })}>
          <option value="">Semua {f.label === 'PIC' ? 'PIC' : f.label.toLowerCase()}</option>
          {list.map((o) => <option key={o.id} value={o.id}>{o.label}{o.active ? '' : ' (nonaktif)'}</option>)}
          <option value={f.unset[0]}>{f.unset[1]}</option>
          {!known && <option value={value}>Nilai tidak dikenal</option>}
        </select>
      </label>
    );
  };
  const byKey = (k: DashboardFilterKey) => FILTERS.find((f) => f.key === k)!;

  return (
    <>
      <PageHeader title="Laporan" description="Rekap operasional per periode · bukan laporan regulator"
        action={<Button variant="primary" onClick={refresh} disabled={!!dateError}><Icon name="reports" size={16} /> Pratinjau</Button>} />

      <section className="pcard rep-params" aria-label="Parameter laporan">
        <form className="bd" onSubmit={(e) => { e.preventDefault(); refresh(); }}>
          <div className="formgrid rep-grid">
            <div className="field rep-period">
              <label htmlFor="rep-preset">Periode</label>
              <select id="rep-preset" className="field-control" value={preset} onChange={(e) => pickPreset(e.target.value)}
                aria-describedby={preset === 'custom' ? undefined : 'rep-preset-range'}>
                {REPORT_PRESETS.map((o) => <option key={o.id} value={o.id}>{o.label}</option>)}
                <option value="custom">Rentang kustom</option>
              </select>
              {preset !== 'custom' && <span id="rep-preset-range" className="sr-only">{rangeLabel(params.periodStart, params.periodEnd)}</span>}
              {preset === 'custom' && <div className="rep-dates">
                <label>
                  <span className="sr-only">Tanggal awal</span>
                  <input type="date" className="field-control" value={start} max={end || undefined} aria-label="Tanggal awal"
                    aria-invalid={dateError ? true : undefined} aria-describedby={dateError ? 'rep-date-error' : undefined}
                    onChange={(e) => setStart(e.target.value)} onBlur={applyPeriod} />
                </label>
                <span aria-hidden="true">–</span>
                <label>
                  <span className="sr-only">Tanggal akhir</span>
                  <input type="date" className="field-control" value={end} min={start || undefined} aria-label="Tanggal akhir"
                    aria-invalid={dateError ? true : undefined} aria-describedby={dateError ? 'rep-date-error' : undefined}
                    onChange={(e) => setEnd(e.target.value)} onBlur={applyPeriod} />
                </label>
              </div>}
              {preset === 'custom' && dateError && <span id="rep-date-error" className="field-error" role="alert">{dateError}</span>}
            </div>
            <label className="field">
              Dasar waktu
              <select className="field-control" value={params.basis}
                onChange={(e) => go({ ...params, basis: e.target.value === 'resolved' ? 'resolved' : 'received' })}>
                <option value="received">Tanggal diterima</option>
                <option value="resolved">Tanggal diselesaikan</option>
              </select>
            </label>
            {filterSelect(byKey('channel'))}
            {filterSelect(byKey('category'))}
            <details className="full rep-more" open={morePicked || undefined}>
              <summary>Filter lanjutan{morePicked ? ' · aktif' : ''}</summary>
              <div className="formgrid">
                <label className="field">
                  Jenis laporan
                  <select className="field-control" value="operational" aria-describedby="rep-type-hint" onChange={() => undefined}>
                    <option value="operational">Rekap operasional</option>
                    <option value="bi_regulator" disabled>Persiapan laporan BI (belum tersedia)</option>
                  </select>
                  <span id="rep-type-hint" className="field-hint">
                    Persiapan laporan BI dinonaktifkan: {(bi?.reason ?? 'pemetaan regulator belum tersedia').replace(/\.$/, '')}.
                  </span>
                </label>
                {filterSelect(byKey('owner'))}
                {filterSelect(byKey('priority'))}
                <fieldset className="report-status full">
                  <legend>Status (kosong = semua)</legend>
                  {REPORT_STATUSES.map((c) => (
                    <label key={c} className="check">
                      <input type="checkbox" checked={params.status.includes(c)} onChange={() => toggleStatus(c)} />
                      {p?.byStatus.find((s) => s.status.code === c)?.status.label ?? STATUS_LABELS[c]}
                    </label>
                  ))}
                </fieldset>
              </div>
            </details>
          </div>
          <div className="rep-actions">
            <Button type="submit" disabled={!!dateError}>Tampilkan pratinjau</Button>
            <Button variant="primary" onClick={() => exportFile('xlsx')} disabled={exportBlocked} aria-describedby="rep-export-state">
              {exporting === 'xlsx' ? 'Menyiapkan Excel…' : 'Buat ekspor (XLSX)'}
            </Button>
            <Button onClick={() => exportFile('csv')} disabled={exportBlocked} aria-describedby="rep-export-state">
              {exporting === 'csv' ? 'Menyiapkan CSV…' : 'Ekspor CSV'}
            </Button>
            {(filtered || params.basis !== 'received') && <Button variant="ghost" onClick={reset}>Reset</Button>}
            <span className="hintline rep-limit">
              Batas {p ? num(p.exportMaxRows) : '—'} baris per ekspor · berkas kedaluwarsa setelah masa berlaku
            </span>
          </div>
          <p id="rep-export-state" ref={exportStatus} tabIndex={-1} className="hintline rep-export-state" role="status" aria-live="polite">
            {exporting ? 'Status: membangun file…'
              : p && !p.exportable ? 'Ekspor tidak tersedia: perbaiki item validasi yang menghambat.'
                : lastRun ? runSummary(lastRun, tz)
                  : ''}
            {!exporting && lastRun?.downloadable && <> · <a href={api.reportContentUrl(workspace.id, lastRun.id)} download={lastRun.fileName ?? undefined}>Unduh {lastRun.fileName}</a></>}
          </p>
          {exportError && <ErrorState title={exportError.title} body={exportError.body} requestId={exportError.requestId} />}
        </form>
      </section>

      <p className="sr-only" role="status">{shown && p ? `Pratinjau laporan ${rangeLabel(p.header.periodStart, p.header.periodEnd)} dimuat.` : ''}</p>
      {preview.status === 'error' && (
        <PreviewError error={preview.error} onRetry={() => setAttempt((n) => n + 1)} onReset={reset} />
      )}
      {!shown && preview.status !== 'error' && (
        <section className="pcard rep-block rep-empty" aria-label="Pratinjau laporan">
          <p>Belum ada pratinjau. Pilih periode lalu tekan “Pratinjau”.</p>
        </section>
      )}
      {shown && preview.status === 'loading' && <LoadingState label="Memuat pratinjau laporan…" />}
      {shown && p && <PreviewBody p={p} filtered={filtered} blocking={blocking} />}

      <section className="pcard rep-runs" aria-labelledby="rep-runs">
        <div className="hd">
          <h2 id="rep-runs">Ekspor terakhir</h2>
          <div className="r"><Button variant="ghost" className="btn-small" onClick={() => setRunsAttempt((n) => n + 1)}>Muat ulang</Button></div>
        </div>
        {runs.status === 'loading' && <div className="bd"><LoadingState label="Memuat riwayat ekspor…" rows={2} /></div>}
        {runs.status === 'error' && (
          <div className="bd">
            <ErrorState title="Riwayat ekspor belum dapat dimuat" body={exportErrorText(runs.error).body}
              requestId={runs.error instanceof ApiError ? runs.error.requestId : null} onRetry={() => setRunsAttempt((n) => n + 1)} />
          </div>
        )}
        {runs.status === 'success' && (runs.data.items.length === 0
          ? <div className="bd"><p className="empty-mini"><b>Belum ada ekspor</b>Ekspor operasional Excel / CSV yang dibuat di perusahaan ini muncul di sini.</p></div>
          : (
            <div className="ptable">
              <DataTable caption="Riwayat ekspor laporan" columns={runColumns(tz, (id) => api.reportContentUrl(workspace.id, id))}
                rows={runs.data.items} rowKey={(r) => r.id} empty={{ title: 'Belum ada ekspor', body: '' }} />
            </div>
          ))}
      </section>

      <section className="pcard callout-amber" aria-labelledby="rep-bi">
        <div className="bd">
          <h2 id="rep-bi">Laporan pengaduan untuk regulator (BI) belum tersedia</h2>
          <p className="hintline">
            {bi?.reason ?? 'Pemetaan format regulator belum ditetapkan, jadi tidak ada kode BI yang dibuat atau ditebak.'}{' '}
            Ekspor di halaman ini hanya rekap operasional, berlabel OPERASIONAL / NON-REGULATOR.
            {p && <> Perusahaan: {p.header.companyName} ({p.header.companyCode}) · kode pelapor {p.header.reporterCode ?? 'belum dikonfigurasi'}.</>}
          </p>
        </div>
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
    if (e.status === 403) return { title: 'Akses ditolak', body: 'Anda tidak memiliki izin ekspor di perusahaan ini.', requestId };
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

/** Tickets without a category, only when the category breakdown covers every ticket; otherwise null ("Belum tersedia"). */
function uncategorized(p: ReportPreview): number | null {
  const sum = p.byCategory.reduce((n, r) => n + r.count, 0);
  if (sum !== p.totals.tickets) return null;
  return p.byCategory.find((r) => r.id === null)?.count ?? 0;
}

const pct = (n: number, total: number) => (total > 0 ? `${Math.round((n / total) * 100)}%` : '—');

function PreviewBody({ p, filtered, blocking }: { p: ReportPreview; filtered: boolean; blocking: number }) {
  const t = p.totals;
  const tz = p.header.timezone;
  const basisText = p.header.basis === 'resolved' ? 'diselesaikan pada periode' : 'diterima pada periode';
  const range = rangeLabel(p.header.periodStart, p.header.periodEnd);
  const noCat = uncategorized(p);
  return (
    <>
      <div className="g4 rep-kpis">
        <div className="kpi2 kpi-active"><span className="lbl">Tiket dalam periode</span><strong className="val">{num(t.tickets)}</strong><span className="hint">{range} · {p.header.basisLabel.toLowerCase()} · = baris ekspor</span></div>
        <div className="kpi2 kpi-resolved"><span className="lbl">Pernah diselesaikan</span><strong className="val">{num(t.resolvedTickets)}</strong><span className="hint">Tiket unik · {num(t.resolutionCycles)} siklus penyelesaian dalam periode</span></div>
        <div className={`kpi2${t.unassigned > 0 ? ' alert' : ''}`}><span className="lbl">Tanpa PIC</span><strong className="val">{num(t.unassigned)}</strong><span className="hint">Belum ditugaskan saat ini</span></div>
        <div className="kpi2 kpi-nocat"><span className="lbl">Tanpa kategori</span>
          {noCat === null ? <strong className="val na">Belum tersedia</strong> : <strong className="val">{num(noCat)}</strong>}
          <span className="hint">Kualitas klasifikasi</span></div>
      </div>

      {p.validation.length > 0 && (
        <details className="pcard rep-block rep-rows rep-validation">
          <summary className="hd">
            <h2 id="rep-validation">{num(p.validation.length)} catatan validasi</h2>
            <span className="sub">{blocking > 0 ? `${num(blocking)} menghambat ekspor` : 'tidak menghambat ekspor'} · dicek sebelum ekspor</span>
            <span className="r rep-toggle" aria-hidden="true"><Icon name="chevron" size={16} /></span>
          </summary>
          <ul className="plain bd">
            {p.validation.map((v) => (
              <li className="row activity" key={v.code}>
                <Badge tone={LEVEL_TONE[v.level] ?? 'neutral'}>{LEVEL_LABEL[v.level] ?? v.level}</Badge>
                <span>{v.message}</span>
              </li>
            ))}
          </ul>
        </details>
      )}

      {t.tickets === 0 ? (
        <section className="pcard rep-block"><div className="bd">
          <EmptyState title="Tidak ada tiket pada periode ini"
            body={filtered ? 'Tidak ada tiket yang cocok dengan kombinasi filter dan periode ini.' : `Tidak ada tiket ${basisText} di ${p.header.workspaceCode}.`} />
        </div></section>
      ) : (
        <>
          <section className="pcard rep-block" aria-labelledby="rep-status">
            <div className="hd">
              <h2 id="rep-status">Rekap per status</h2>
              <span className="sub">{range} · dasar waktu {p.header.basisLabel.toLowerCase()} · status saat ini</span>
            </div>
            <div className="ptable" role="region" aria-label="Rekap per status" tabIndex={0}>
              <table>
                <caption className="sr-only">Rekap per status</caption>
                <thead><tr><th scope="col">Status</th><th scope="col">Jumlah</th><th scope="col">Porsi</th></tr></thead>
                <tbody>
                  {p.byStatus.map((s) => (
                    <tr key={s.status.code}>
                      <td><Badge tone={statusTone(s.status)}>{s.status.label}</Badge></td>
                      <td><b>{num(s.count)}</b></td><td>{pct(s.count, t.tickets)}</td>
                    </tr>
                  ))}
                </tbody>
                <tfoot><tr><th scope="row">Total</th><td><b>{num(t.tickets)}</b></td><td>100%</td></tr></tfoot>
              </table>
            </div>
          </section>
          <div className="g2 rep-breakdowns">
            <Breakdown id="rep-priority" title="Per prioritas" rows={p.byPriority} none="Tanpa prioritas" total={t.tickets} />
            <Breakdown id="rep-channel" title="Per kanal" rows={p.byChannel} none="Tanpa kanal" total={t.tickets} />
            <Breakdown id="rep-category" title="Per kategori" rows={p.byCategory} none="Tanpa kategori" total={t.tickets} />
            <Breakdown id="rep-owner" title="Per PIC" rows={p.byOwner} none="Belum ditugaskan" total={t.tickets} />
          </div>
          <details className="pcard rep-block rep-rows">
            <summary className="hd">
              <h2 id="rep-rows">Pratinjau baris</h2>
              <span className="sub">{num(p.rows.length)} dari {num(t.tickets)} baris pertama (diterima, lalu nomor) · total dihitung server dari seluruh baris</span>
              <span className="r rep-toggle" aria-hidden="true"><Icon name="chevron" size={16} /></span>
            </summary>
            <div className="ptable">
              <DataTable caption="Pratinjau baris laporan" columns={ROW_COLUMNS} rows={p.rows} rowKey={(r) => r.ticketId}
                empty={{ title: 'Tidak ada baris', body: 'Tidak ada tiket pada laporan ini.' }} />
            </div>
          </details>
        </>
      )}
      <p className="hintline rep-source">
        {p.notice} · per {instantLabel(p.asOf, tz)} {tzLabel(tz)} · rentang UTC {p.header.startAt} s.d. sebelum {p.header.endAt}
      </p>
    </>
  );
}

function Breakdown({ id, title, rows, none, total }: { id: string; title: string; rows: readonly ReportBreakdown[]; none: string; total: number }) {
  return (
    <section className="pcard" aria-labelledby={id}>
      <div className="hd"><h2 id={id}>{title}</h2></div>
      <div className="bd">
        {rows.length === 0 ? <p className="empty-mini">Belum tersedia</p> : (
          <ul className="sbars">
            {rows.map((r) => (
              <li className="sbar" key={r.id ?? 'none'}>
                <span>{r.label ?? none}</span>
                <span className="track" aria-hidden="true"><span className="fill" style={{ width: total > 0 ? `${(r.count / total) * 100}%` : 0 }} /></span>
                <b>{num(r.count)}</b>
              </li>
            ))}
          </ul>
        )}
      </div>
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
  return r.downloadable ? { label: 'Tersedia', tone: 'ok' } : { label: 'Selesai · tanpa file', tone: 'neutral' };
}

function runSummary(r: ReportRun, tz: string): string {
  const s = runState(r).label;
  if (r.state === 'failed') return `Status: ${s}. Tidak ada file yang dibuat.`;
  return `Status: ${s} · ${num(r.rowCount ?? 0)} baris${r.expiresAt ? ` · berlaku s.d. ${instantLabel(r.expiresAt, tz)}` : ''}`;
}

function runColumns(tz: string, url: (id: string) => string): readonly Column<ReportRun>[] {
  return [
    // CP-UI3b: the columns of prototype 07; requester, SHA-256 and expiry stay readable as a hover title.
    {
      key: 'name', header: 'Nama',
      cell: (r) => <b title={[`Diminta ${r.requestedBy.label}`, r.artifactSha256 ? `SHA-256 ${r.artifactSha256}` : '', r.expiresAt ? `Berlaku s.d. ${instantLabel(r.expiresAt, tz)}` : ''].filter(Boolean).join(' · ')}>Operasional · {rangeLabel(r.periodStart, r.periodEnd)}</b>,
    },
    { key: 'basis', header: 'Dasar', cell: (r) => (r.basis === 'resolved' ? 'diselesaikan' : 'diterima') },
    { key: 'format', header: 'Format', cell: (r) => (r.format === 'xlsx' ? 'XLSX' : 'CSV') },
    { key: 'rows', header: 'Baris', cell: (r) => (r.rowCount === null ? '—' : num(r.rowCount)) },
    { key: 'state', header: 'Status', cell: (r) => { const s = runState(r); return <Badge tone={s.tone}>{s.label}</Badge>; } },
    { key: 'created', header: 'Dibuat', cell: (r) => instantLabel(r.createdAt, tz) },
    {
      key: 'download', header: 'Unduh',
      cell: (r) => (r.downloadable
        ? <a className="btn btn-small" href={url(r.id)} download={r.fileName ?? undefined} aria-label={`Unduh ${r.fileName ?? FORMAT_LABELS[r.format]}`}>Unduh</a>
        : <span className="muted">{r.expired ? 'Kedaluwarsa' : 'Tidak tersedia'}</span>),
    },
  ];
}
