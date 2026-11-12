import { useRef, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type { Dashboard, DashboardPeriodKey, TicketFilter, Workspace } from '../api/types';
import { barPercent, channelSlices, dataQuality, donutGradient, resolvedKpiLabel } from '../app/dashboardView';
import { ageLabel, instantLabel, num, PERIOD_LABELS, rangeLabel, tzLabel } from '../app/format';
import { navigate, useDashboard } from '../app/hooks';
import {
  dashboardQuery,
  drilldownHash,
  hasDashboardFilter,
  NEW_TICKET_FLAG,
  parseDashboardQuery,
  toHash,
  withDetail,
  type DashboardFilterKey,
  type DashboardParams,
} from '../app/routes';
import { Badge, statusTone } from '../components/Badge';
import { Button } from '../components/Button';
import { Icon } from '../components/Icon';
import { PageHeader } from '../components/PageHeader';
import { EmptyState, ErrorState, ForbiddenState, NotFoundState } from '../components/States';

const PERIODS: readonly DashboardPeriodKey[] = ['today', 'last_7_days', 'this_month'];

type Options = Dashboard['filterOptions'];

const FILTER_FIELDS: readonly {
  key: DashboardFilterKey;
  label: string;
  list: keyof Options;
  unset: { value: string; label: string };
}[] = [
  { key: 'owner', label: 'PIC', list: 'owners', unset: { value: 'unassigned', label: 'Belum ditugaskan' } },
  { key: 'channel', label: 'Kanal', list: 'channels', unset: { value: 'none', label: 'Tanpa kanal' } },
  { key: 'category', label: 'Kategori', list: 'categories', unset: { value: 'none', label: 'Tanpa kategori' } },
  { key: 'priority', label: 'Prioritas', list: 'priorities', unset: { value: 'none', label: 'Tanpa prioritas' } },
];

/**
 * O01: whole-scope aggregates from the server for the active workspace. Period and shared
 * filters live in the URL (`#/w/<ws>/dashboard?period=…&owner=…`), so reload, back and a shared
 * link give the same numbers; a workspace switch drops the query and so resets the filters.
 */
export function DashboardPage({ api, workspace, canReports, canCreate = false, query = '' }: {
  api: Api;
  workspace: Workspace;
  canReports: boolean;
  /** ticket.create: shows "Tiket baru" (same rule as the sidebar entry). */
  canCreate?: boolean;
  query?: string;
}) {
  const params = parseDashboardQuery(query);
  const [attempt, setAttempt] = useState(0);
  const periodRef = useRef<HTMLSelectElement>(null);
  const filterSummaryRef = useRef<HTMLElement>(null);
  const state = useDashboard(api, workspace.id, params, attempt);
  const d = state.status === 'success' ? state.data : null;
  const tz = d?.period.timezone ?? workspace.timezone;
  // Options of this workspace from the last answer: the selects stay usable while the next loads.
  const options = useRef<Options | null>(null);
  if (d) options.current = d.filterOptions;
  const filtered = hasDashboardFilter(params);

  const go = (next: DashboardParams) =>
    navigate(toHash({ workspaceId: workspace.id, page: 'dashboard', query: dashboardQuery(next) }));
  const setFilter = (k: DashboardFilterKey, v: string) =>
    go({ ...params, filters: { ...params.filters, [k]: v || null } });
  const reset = () => {
    periodRef.current?.focus();
    go({ period: params.period, filters: { owner: null, channel: null, category: null, priority: null } });
  };

  const scopeText = filtered ? `${workspace.name} (terfilter)` : workspace.name;
  return (
    <>
      <PageHeader title="Dashboard"
        description={d
          ? `Ringkasan beban kerja ${PERIOD_LABELS[d.period.key].toLowerCase()} · ${rangeLabel(d.period.startDate, d.period.endDate)} · ${scopeText}`
          : `Ringkasan beban kerja ${scopeText}`}
        action={(canReports || canCreate) ? (
          <div className="ph-acts">
            {canReports && <a className="btn" href={toHash({ workspaceId: workspace.id, page: 'reports' })}><Icon name="reports" size={16} /> Lihat laporan</a>}
            {canCreate && <a className="btn btn-primary" href={toHash({ workspaceId: workspace.id, page: 'tickets', query: `${NEW_TICKET_FLAG}=1` })}><Icon name="plus" size={16} /> Tiket baru</a>}
          </div>
        ) : undefined} />
      <details className="dashboard-filter-panel" open={filtered || undefined}>
        <summary ref={filterSummaryRef}><span className="filter-heading"><Icon name="filter" size={16} /> Periode &amp; filter</span><span className="filter-summary">{PERIOD_LABELS[params.period]}{filtered ? " · Filter aktif" : " · Semua tiket"}<Icon name="chevron" size={14} /></span></summary>
      <div className="toolbar row filterbar dash-filters" role="group" aria-label="Periode dan filter dashboard">
        <label className="period">
          Periode
          <select ref={periodRef} className="field-control" value={params.period}
            onChange={(e) => go({ ...params, period: e.target.value as DashboardPeriodKey })}>
            {PERIODS.map((k) => (
              <option key={k} value={k}>
                {PERIOD_LABELS[k]}{d && d.period.key === k ? ` · ${rangeLabel(d.period.startDate, d.period.endDate)}` : ''}
              </option>
            ))}
          </select>
        </label>
        {FILTER_FIELDS.map((f) => {
          const value = params.filters[f.key] ?? '';
          const list = options.current?.[f.list] ?? [];
          const known = value === '' || value === f.unset.value || list.some((o) => o.id === value);
          return (
            <label className="period" key={f.key}>
              {f.label}
              <select className="field-control" value={value} disabled={!options.current && value === ''}
                onChange={(e) => setFilter(f.key, e.target.value)}>
                <option value="">Semua</option>
                {list.map((o) => (
                  <option key={o.id} value={o.id}>{o.label}{o.active ? '' : ' (nonaktif)'}</option>
                ))}
                <option value={f.unset.value}>{f.unset.label}</option>
                {!known && <option value={value}>Nilai tidak dikenal</option>}
              </select>
            </label>
          );
        })}
        {filtered && <Button variant="ghost" onClick={reset}>Reset filter</Button>}
        <span className="muted tiny">Waktu {tzLabel(tz)} · basis tiap angka tertulis pada kartunya</span>
        {d && !d.slaConfigured && <Badge>SLA belum dikonfigurasi</Badge>}
      </div>
      </details>
      <p className="sr-only" role="status">
        {d ? `Dashboard ${PERIOD_LABELS[d.period.key]}${filtered ? ' dengan filter' : ''} dimuat.` : ''}
      </p>
      {state.status === 'loading' && <DashboardSkeleton period={params.period} />}
      {state.status === 'error' && (
        <DashboardError error={state.error} onReset={filtered ? reset : null} onRetry={() => {
          // Retry removes the error button; the filter summary remains visible when collapsed.
          filterSummaryRef.current?.focus();
          setAttempt((n) => n + 1);
        }} />
      )}
      {d && <DashboardBody d={d} filtered={filtered} />}
    </>
  );
}

function DashboardSkeleton({ period }: { period: DashboardPeriodKey }) {
  return (
    <div role="status" aria-live="polite">
      <span className="sr-only">Memuat dashboard…</span>
      <div className="kpis" aria-hidden="true">
        {['Tiket aktif', 'Belum ditugaskan', 'Menunggu pelanggan', resolvedKpiLabel(period)].map((l) => (
          <div className="kpi" key={l}><span className="muted">{l}</span><strong>—</strong><div className="skeleton" /></div>
        ))}
      </div>
      <div className="grid2" aria-hidden="true">
        <div className="card"><div className="skeleton" /><div className="skeleton" /><div className="skeleton" /></div>
        <div className="card"><div className="skeleton" /><div className="skeleton" /></div>
      </div>
    </div>
  );
}

function DashboardError({ error, onRetry, onReset }: {
  error: unknown;
  onRetry: () => void;
  onReset: (() => void) | null;
}) {
  if (error instanceof ApiError && error.status === 400 && onReset) {
    return <ErrorState title="Filter tidak valid untuk workspace ini"
      body="Nilai filter pada alamat halaman tidak dikenali di workspace aktif. Tidak ada angka yang ditampilkan."
      requestId={error.requestId} onRetry={onReset} retryLabel="Reset filter" />;
  }
  if (error instanceof ApiError && error.status === 403) return <ForbiddenState />;
  if (error instanceof ApiError && error.status === 404) return <NotFoundState />;
  if (error instanceof ApiError && error.status === 401) {
    return <ErrorState title="Sesi berakhir" body="Sesi Anda tidak lagi aktif. Muat ulang halaman untuk masuk lagi."
      requestId={error.requestId} />;
  }
  const body = error instanceof ApiError && error.code === 'network'
    ? 'Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.'
    : 'Angka dashboard tidak ditampilkan karena permintaan gagal. Tidak ada data lama yang dipakai.';
  return <ErrorState title="Dashboard belum dapat dimuat" body={body}
    requestId={error instanceof ApiError ? error.requestId : null} onRetry={onRetry} />;
}

function DashboardBody({ d, filtered }: { d: Dashboard; filtered: boolean }) {
  const ws = d.workspaceId;
  const t = d.totals;
  const periodText = PERIOD_LABELS[d.period.key];
  const drill = (f: TicketFilter | null) => (f ? drilldownHash(ws, f) : null);
  const scope = filtered ? `${d.workspaceCode} (terfilter)` : d.workspaceCode;
  const kpis = [
    { key: 'active', label: 'Tiket aktif', value: t.active, hint: 'Semua status berjalan saat ini', href: drill(d.drilldown.active) },
    { key: 'unassigned', label: 'Belum ditugaskan', value: t.unassigned, hint: 'Aktif tanpa PIC · perlu diambil CS', href: drill(d.drilldown.unassigned) },
    { key: 'customer', label: 'Menunggu pelanggan', value: t.pendingCustomer, hint: 'Bergantung balasan pelanggan', href: drill(d.drilldown.pendingCustomer) },
    {
      key: 'resolved', label: resolvedKpiLabel(d.period.key), value: t.resolvedInPeriod,
      hint: `Peristiwa penyelesaian · ${num(t.resolvedTicketsInPeriod)} tiket unik`,
      href: drill(d.drilldown.resolvedInPeriod),
    },
  ];

  return (
    <>
      <div className="g4 dash-kpis">
        {kpis.map((k) => {
          const cls = `kpi2 kpi-${k.key}${k.key === 'unassigned' && k.value > 0 ? ' alert' : ''}`;
          const body = (<>
            <span className="lbl">{k.label}</span>
            <strong className="val">{num(k.value)}</strong>
            <span className="hint">{k.href ? k.hint : `${k.hint} · tidak berlaku untuk filter PIC`}</span>
          </>);
          return k.href
            ? <a className={cls} key={k.key} href={k.href}>{body}</a>
            : <div className={cls} key={k.key}>{body}</div>;
        })}
      </div>
      {t.tickets === 0 ? (
        filtered ? (
          <EmptyState title="Tidak ada tiket yang cocok"
            body={`Tidak ada tiket di ${d.workspaceCode} untuk kombinasi filter ini. Ubah atau reset filter.`} />
        ) : (
          <EmptyState title="Belum ada tiket"
            body={`Belum ada tiket di ${d.workspaceCode}. Ringkasan muncul setelah tiket pertama diterima.`} />
        )
      ) : (
        <>
          <div className="g2">
            <section className="pcard" aria-labelledby="dash-status">
              <header className="hd">
                <h2 id="dash-status">Sebaran status</h2>
                <span className="sub">seluruh {num(t.tickets)} tiket {scope}</span>
                {!d.slaConfigured && <span className="r"><span className="chip-tag">tanpa SLA</span></span>}
              </header>
              <div className="bd">
                <StatusBars d={d} />
                <p className="foot-note">Status saat ini · tidak dibatasi periode. Klik status untuk membuka daftarnya.</p>
              </div>
            </section>
            <section className="pcard" aria-labelledby="dash-channel">
              <header className="hd">
                <h2 id="dash-channel">Kanal masuk</h2>
                <span className="sub">{num(t.receivedInPeriod)} tiket diterima {periodText.toLowerCase()}</span>
              </header>
              <div className="bd"><ChannelDonut d={d} /></div>
            </section>
          </div>

          <div className="g2">
            <section className="pcard" aria-labelledby="dash-attention">
              <header className="hd">
                <h2 id="dash-attention">Perlu perhatian</h2>
                <span className="sub">aktif prioritas Mendesak/Tinggi · {num(d.attention.items.length)} dari {num(d.attention.total)}</span>
                {d.drilldown.attention && (
                  <span className="r"><a className="btn btn-small" href={drilldownHash(ws, d.drilldown.attention)}>Lihat semua</a></span>
                )}
              </header>
              <AttentionTable d={d} />
            </section>
            <section className="pcard" aria-labelledby="dash-quality">
              <header className="hd"><h2 id="dash-quality">Kualitas data</h2></header>
              <div className="bd">
                <QualityBars d={d} />
                <p className="foot-note">Kendali mutu klasifikasi sebelum laporan dibuat; basis tiap baris tertulis di bawah labelnya.</p>
                <h3 className="eyebrow-h" id="dash-activity">Aktivitas terakhir</h3>
                <div className="empty-mini" aria-labelledby="dash-activity">
                  <strong>Belum tersedia</strong>
                  <span>Ringkasan aktivitas lintas tiket belum disediakan server. Riwayat per tiket ada di detail tiket.</span>
                </div>
              </div>
            </section>
          </div>
        </>
      )}
      <p className="tiny muted source">
        Sumber: API ITSM, agregat server seluruh {scope} · per {instantLabel(d.asOf, d.period.timezone)}{' '}
        {tzLabel(d.period.timezone)}
      </p>
    </>
  );
}

function StatusBars({ d }: { d: Dashboard }) {
  const max = Math.max(0, ...d.statusDistribution.map((s) => s.count));
  return (
    <ul className="sbars" aria-label="Sebaran tiket menurut status">
      {d.statusDistribution.map((s) => (
        <li className="sbar" key={s.status.code}>
          <a href={drilldownHash(d.workspaceId, s.filter)}>{s.status.label}</a>
          <span className="track" aria-hidden="true">
            <span className={s.status.code === 'resolved' ? 'fill ok' : 'fill'} style={{ width: `${barPercent(s.count, max)}%` }} />
          </span>
          <b>{num(s.count)}</b>
        </li>
      ))}
    </ul>
  );
}

function ChannelDonut({ d }: { d: Dashboard }) {
  const slices = channelSlices(d.channels);
  const total = slices.reduce((n, s) => n + s.count, 0);
  if (total === 0) return <p className="muted">Tidak ada tiket diterima pada periode ini.</p>;
  const summary = slices.map((s) => `${s.label} ${num(s.count)}`).join(', ');
  return (
    <div className="donut">
      <div className="ring" role="img" aria-label={`Kanal masuk: ${summary}`} style={{ background: donutGradient(slices) }}>
        <span aria-hidden="true">{num(total)}</span>
      </div>
      <ul className="legend">
        {slices.map((s) => {
          const row = d.channels.find((c) => (c.id ?? 'none') === s.key);
          return (
            <li key={s.key}>
              <i aria-hidden="true" style={{ background: s.color }} />
              {row ? <a href={drilldownHash(d.workspaceId, row.filter)}>{s.label}</a> : s.label}
              <b>{num(s.count)}</b>
            </li>
          );
        })}
      </ul>
    </div>
  );
}

function QualityBars({ d }: { d: Dashboard }) {
  return (
    <ul className="qbars">
      {dataQuality(d).map((q) => (
        <li key={q.key}>
          <span className="q-lbl">{q.label}<small>{q.basis}</small></span>
          <span className="track" aria-hidden="true">
            <span className="fill warn" style={{ width: `${barPercent(q.value, q.of)}%` }} />
          </span>
          <b>{num(q.value)}</b>
        </li>
      ))}
    </ul>
  );
}

function AttentionTable({ d }: { d: Dashboard }) {
  if (d.attention.items.length === 0) {
    return (
      <div className="bd">
        {d.drilldown.attention
          ? <EmptyState title="Tidak ada tiket mendesak" body="Tidak ada tiket aktif berprioritas Mendesak atau Tinggi." />
          : <EmptyState title="Di luar filter prioritas" body="Filter prioritas aktif tidak mencakup Mendesak atau Tinggi." />}
      </div>
    );
  }
  return (
    <div className="ptable" role="region" aria-label="Tiket perlu perhatian" tabIndex={0}>
      <table>
        <caption className="sr-only">Tiket perlu perhatian · Mendesak dulu, lalu yang paling lama diterima</caption>
        <thead><tr><th scope="col">Nomor</th><th scope="col">Subjek</th><th scope="col">Status</th><th scope="col">Umur</th></tr></thead>
        <tbody>
          {d.attention.items.map((r) => (
            <tr key={r.id}>
              <td><a className="tno" href={toHash({ workspaceId: d.workspaceId, page: 'tickets', query: withDetail('', r.id, true) })}>{r.displayNumber}</a></td>
              <td><span className="tsub">{r.subject}</span><span className="tmeta">{[r.priority?.label, r.owner?.label ?? 'Belum ditugaskan'].filter(Boolean).join(' · ')}</span></td>
              <td><Badge tone={statusTone(r.status)}>{r.status.label}</Badge></td>
              <td className="nowrap"><b>{ageLabel(r.ageSeconds)}</b></td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
