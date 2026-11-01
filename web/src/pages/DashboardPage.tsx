import { useRef, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type {
  Dashboard,
  DashboardAttentionTicket,
  DashboardPeriodKey,
  TicketFilter,
  TicketStatus,
  Workspace,
} from '../api/types';
import { ageLabel, instantLabel, num, PERIOD_LABELS, rangeLabel, tzLabel } from '../app/format';
import { useDashboard } from '../app/hooks';
import { initials } from '../app/labels';
import { navigate } from '../app/hooks';
import {
  dashboardQuery,
  drilldownHash,
  hasDashboardFilter,
  parseDashboardQuery,
  toHash,
  type DashboardFilterKey,
  type DashboardParams,
} from '../app/routes';
import { Badge, type BadgeTone } from '../components/Badge';
import { BarList, TrendChart } from '../components/Charts';
import { Button } from '../components/Button';
import { DataTable, type Column } from '../components/DataTable';
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
export function DashboardPage({ api, workspace, canReports, uat, query = '' }: {
  api: Api;
  workspace: Workspace;
  canReports: boolean;
  uat: boolean;
  query?: string;
}) {
  const params = parseDashboardQuery(query);
  const [attempt, setAttempt] = useState(0);
  const periodRef = useRef<HTMLSelectElement>(null);
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

  return (
    <>
      <PageHeader eyebrow="Operations overview" title="Dashboard"
        description={`Ringkasan penanganan pengaduan untuk perusahaan ${workspace.name}.`}
        action={canReports
          ? <a className="btn" href={toHash({ workspaceId: workspace.id, page: 'reports' })}>Lihat laporan ↗</a>
          : undefined} />
      <div className="toolbar row" role="group" aria-label="Periode dan filter dashboard">
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
      <p className="sr-only" role="status">
        {d ? `Dashboard ${PERIOD_LABELS[d.period.key]}${filtered ? ' dengan filter' : ''} dimuat.` : ''}
      </p>
      {state.status === 'loading' && <DashboardSkeleton />}
      {state.status === 'error' && (
        <DashboardError error={state.error} onReset={filtered ? reset : null} onRetry={() => {
          // The retry button unmounts while loading; keep keyboard focus on a stable control.
          periodRef.current?.focus();
          setAttempt((n) => n + 1);
        }} />
      )}
      {d && <DashboardBody d={d} uat={uat} filtered={filtered} />}
    </>
  );
}

function DashboardSkeleton() {
  return (
    <div role="status" aria-live="polite">
      <span className="sr-only">Memuat dashboard…</span>
      <div className="kpis" aria-hidden="true">
        {['Tiket aktif', 'Belum ditugaskan', 'Menunggu pelanggan', 'Terselesaikan'].map((l) => (
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
    return <ErrorState title="Sesi berakhir" body="Sesi uji tidak lagi aktif. Muat ulang halaman untuk masuk lagi."
      requestId={error.requestId} />;
  }
  const body = error instanceof ApiError && error.code === 'network'
    ? 'Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.'
    : 'Angka dashboard tidak ditampilkan karena permintaan gagal. Tidak ada data lama yang dipakai.';
  return <ErrorState title="Dashboard belum dapat dimuat" body={body}
    requestId={error instanceof ApiError ? error.requestId : null} onRetry={onRetry} />;
}

export function statusTone(s: Pick<TicketStatus, 'code'>): BadgeTone {
  switch (s.code) {
    case 'new': return 'blue';
    case 'pending_customer':
    case 'pending_engineering': return 'waiting';
    case 'resolved': return 'ok';
    default: return 'neutral';
  }
}

function DashboardBody({ d, uat, filtered }: { d: Dashboard; uat: boolean; filtered: boolean }) {
  const ws = d.workspaceId;
  const t = d.totals;
  const periodText = PERIOD_LABELS[d.period.key];
  const drill = (f: TicketFilter | null) => (f ? drilldownHash(ws, f) : null);
  const scope = filtered ? `${d.workspaceCode} (terfilter)` : d.workspaceCode;
  const kpis = [
    { label: 'Tiket aktif', value: t.active, hint: 'Saat ini · semua status nonterminal', href: drill(d.drilldown.active) },
    { label: 'Belum ditugaskan', value: t.unassigned, hint: 'Saat ini · aktif tanpa PIC', href: drill(d.drilldown.unassigned) },
    { label: 'Menunggu pelanggan', value: t.pendingCustomer, hint: 'Saat ini · status menunggu pelanggan', href: drill(d.drilldown.pendingCustomer) },
    {
      label: 'Terselesaikan',
      value: t.resolvedInPeriod,
      hint: `Peristiwa penyelesaian ${periodText.toLowerCase()} · ${num(t.resolvedTicketsInPeriod)} tiket unik`,
      href: drill(d.drilldown.resolvedInPeriod),
    },
  ];

  return (
    <>
      <div className="kpis">
        {kpis.map((k) => k.href ? (
          <a className="kpi" key={k.label} href={k.href}>
            <span className="muted">{k.label}</span>
            <strong>{num(k.value)}</strong>
            <small>{k.hint} <span aria-hidden="true">↗</span></small>
          </a>
        ) : (
          <div className="kpi" key={k.label}>
            <span className="muted">{k.label}</span>
            <strong>{num(k.value)}</strong>
            <small>{k.hint} · tidak berlaku untuk filter PIC</small>
          </div>
        ))}
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
          <div className="grid2">
            <section className="card" aria-labelledby="dash-status">
              <h2 id="dash-status">Di mana tiket sedang berada?</h2>
              <p className="muted tiny">Status saat ini · seluruh {num(t.tickets)} tiket {scope} · tidak dibatasi periode</p>
              <BarList label="Distribusi tiket menurut status" total={t.tickets}
                items={d.statusDistribution.map((s) => ({ key: s.status.code, label: s.status.label, value: s.count }))} />
              <details>
                <summary>Tampilkan angka</summary>
                <ul className="plain">
                  {d.statusDistribution.map((s) => (
                    <li key={s.status.code}><a href={drilldownHash(ws, s.filter)}>{s.status.label}: {num(s.count)}</a></li>
                  ))}
                </ul>
              </details>
            </section>
            <section className="card" aria-labelledby="dash-pic">
              <h2 id="dash-pic">Beban PIC</h2>
              <p className="muted tiny">Semua tiket aktif saat ini per PIC, apa pun tanggal diterimanya · tidak dibatasi periode</p>
              <ul className="plain">
                {d.picLoad.map((p) => {
                  const name = p.displayName ?? 'Belum ditugaskan';
                  return (
                    <li className="row between activity" key={p.userId ?? 'unassigned'}>
                      <a className="pic" href={drilldownHash(ws, p.filter)}>
                        <span className="avatar" aria-hidden="true">{p.userId ? initials(name) : '—'}</span>
                        {name}
                      </a>
                      <span className="row">
                        {p.userId && !p.memberActive && <Badge>Nonaktif</Badge>}
                        <b>{num(p.activeTickets)}</b>
                      </span>
                    </li>
                  );
                })}
              </ul>
            </section>
          </div>

          <section className="card spaced" aria-labelledby="dash-trend">
            <h2 id="dash-trend">Bagaimana volume pengaduan berubah?</h2>
            <p className="tiny muted">
              {periodText} · {rangeLabel(d.period.startDate, d.period.endDate)} · tiket diterima (per tanggal diterima)
              dibandingkan peristiwa penyelesaian (per tanggal selesai)
            </p>
            {d.trend.length > 1 ? (
              <TrendChart label="Tren harian tiket diterima dan terselesaikan" points={d.trend} />
            ) : (
              <BarList label="Tiket diterima dan terselesaikan" total={Math.max(t.receivedInPeriod, t.resolvedInPeriod)}
                items={[
                  { key: 'received', label: 'Diterima', value: t.receivedInPeriod },
                  { key: 'resolved', label: 'Terselesaikan', value: t.resolvedInPeriod },
                ]} />
            )}
            <p className="tiny muted">
              Bukan penjumlahan dua kelompok. Terselesaikan = peristiwa penyelesaian per siklus, termasuk tiket yang
              diterima sebelum periode; pembukaan ulang tidak menghapusnya. {num(t.resolvedInPeriod)} peristiwa dari{' '}
              {num(t.resolvedTicketsInPeriod)} tiket unik.
            </p>
            <details>
              <summary>Tampilkan angka</summary>
              <TrendTable d={d} />
            </details>
          </section>

          <div className="grid3 spaced">
            <DimensionCard id="dash-channel" title="Dari kanal mana tiket masuk?" none="Tanpa kanal"
              rows={d.channels} received={t.receivedInPeriod} periodText={periodText} ws={ws} />
            <DimensionCard id="dash-category" title="Kategori tiket" none="Tanpa kategori"
              rows={d.categories} received={t.receivedInPeriod} periodText={periodText} ws={ws} />
            <section className="card" aria-labelledby="dash-age">
              <h2 id="dash-age">Umur tiket aktif</h2>
              <p className="tiny muted">Tiket aktif saat ini · usia sejak diterima · bukan pelanggaran SLA</p>
              <ul className="plain">
                {d.age.map((a) => (
                  <li className="row between activity" key={a.key}><span>{a.label}</span><b>{num(a.count)}</b></li>
                ))}
              </ul>
            </section>
          </div>

          <section className="card spaced" aria-labelledby="dash-attention">
            <div className="row between">
              <h2 id="dash-attention">Perlu perhatian</h2>
              {d.drilldown.attention && (
                <a className="btn btn-ghost" href={drilldownHash(ws, d.drilldown.attention)}>Semua tiket →</a>
              )}
            </div>
            <p className="tiny muted">
              Aturan operasional, bukan SLA · tiket aktif prioritas Mendesak/Tinggi · {num(d.attention.items.length)} dari{' '}
              {num(d.attention.total)} · Mendesak dulu, lalu yang paling lama diterima
            </p>
            <DataTable caption="Tiket perlu perhatian" columns={ATTENTION_COLUMNS} rows={d.attention.items}
              rowKey={(r) => r.id}
              empty={{ title: 'Tidak ada tiket mendesak', body: 'Tidak ada tiket aktif berprioritas Mendesak atau Tinggi.' }} />
          </section>
        </>
      )}
      <p className="tiny muted source">
        Sumber: API ITSM, agregat server seluruh {scope} · per {instantLabel(d.asOf, d.period.timezone)}{' '}
        {tzLabel(d.period.timezone)}{uat ? ' · data dummy UAT' : ''}
      </p>
    </>
  );
}

function DimensionCard({ id, title, none, rows, received, periodText, ws }: {
  id: string;
  title: string;
  none: string;
  rows: Dashboard['channels'];
  received: number;
  periodText: string;
  ws: string;
}) {
  return (
    <section className="card" aria-labelledby={id}>
      <h2 id={id}>{title}</h2>
      <p className="tiny muted">Tiket diterima {periodText.toLowerCase()}</p>
      {received === 0 ? (
        <p className="muted">Tidak ada tiket diterima pada periode ini.</p>
      ) : (
        <ul className="plain">
          {rows.map((c) => (
            <li className="row between activity" key={c.id ?? 'none'}>
              <a href={drilldownHash(ws, c.filter)}>{c.label ?? none}</a><b>{num(c.count)}</b>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

function TrendTable({ d }: { d: Dashboard }) {
  return (
    <div className="tablebox" role="region" aria-label="Angka tren harian" tabIndex={0}>
      <table>
        <caption>Tiket diterima dan terselesaikan per hari ({tzLabel(d.period.timezone)})</caption>
        <thead><tr><th scope="col">Tanggal</th><th scope="col">Diterima</th><th scope="col">Terselesaikan</th></tr></thead>
        <tbody>
          {d.trend.map((p) => (
            <tr key={p.date}><td>{rangeLabel(p.date, p.date)}</td><td>{num(p.received)}</td><td>{num(p.resolved)}</td></tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

const ATTENTION_COLUMNS: readonly Column<DashboardAttentionTicket>[] = [
  {
    key: 'ticket',
    header: 'Nomor / tiket',
    cell: (r) => <><b>{r.displayNumber}</b><span className="subject muted" title={r.subject}>{r.subject}</span></>,
  },
  { key: 'status', header: 'Status', cell: (r) => <Badge tone={statusTone(r.status)}>{r.status.label}</Badge> },
  { key: 'priority', header: 'Prioritas', cell: (r) => r.priority?.label ?? '—' },
  { key: 'owner', header: 'PIC', cell: (r) => r.owner?.label ?? 'Belum ditugaskan' },
  { key: 'channel', header: 'Kanal', cell: (r) => r.channel?.label ?? '—' },
  { key: 'category', header: 'Kategori', cell: (r) => r.category?.label ?? '—' },
  { key: 'age', header: 'Umur', cell: (r) => ageLabel(r.ageSeconds) },
];
