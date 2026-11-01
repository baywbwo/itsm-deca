import { useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type {
  Dashboard,
  DashboardAttentionTicket,
  DashboardPeriodKey,
  TicketStatus,
  Workspace,
} from '../api/types';
import { ageLabel, instantLabel, num, PERIOD_LABELS, rangeLabel, tzLabel } from '../app/format';
import { useDashboard } from '../app/hooks';
import { initials } from '../app/labels';
import { drilldownHash, toHash } from '../app/routes';
import { Badge, type BadgeTone } from '../components/Badge';
import { BarList, TrendChart } from '../components/Charts';
import { DataTable, type Column } from '../components/DataTable';
import { PageHeader } from '../components/PageHeader';
import { EmptyState, ErrorState, ForbiddenState, NotFoundState } from '../components/States';

const PERIODS: readonly DashboardPeriodKey[] = ['today', 'last_7_days', 'this_month'];

/** O01: whole-scope aggregates from the server for the active workspace and period. */
export function DashboardPage({ api, workspace, canReports, uat }: {
  api: Api;
  workspace: Workspace;
  canReports: boolean;
  uat: boolean;
}) {
  const [period, setPeriod] = useState<DashboardPeriodKey>('today');
  const [attempt, setAttempt] = useState(0);
  const state = useDashboard(api, workspace.id, period, attempt);
  const d = state.status === 'success' ? state.data : null;
  const tz = d?.period.timezone ?? workspace.timezone;

  return (
    <>
      <PageHeader eyebrow="Operations overview" title="Dashboard"
        description={`Ringkasan penanganan pengaduan untuk perusahaan ${workspace.name}.`}
        action={canReports
          ? <a className="btn" href={toHash({ workspaceId: workspace.id, page: 'reports' })}>Lihat laporan ↗</a>
          : undefined} />
      <div className="toolbar row">
        <label className="period">
          Periode
          <select className="field-control" value={period}
            onChange={(e) => setPeriod(e.target.value as DashboardPeriodKey)}>
            {PERIODS.map((k) => (
              <option key={k} value={k}>
                {PERIOD_LABELS[k]}{d && d.period.key === k ? ` · ${rangeLabel(d.period.startDate, d.period.endDate)}` : ''}
              </option>
            ))}
          </select>
        </label>
        <span className="muted tiny">Basis: diterima · Waktu {tzLabel(tz)}</span>
        {d && !d.slaConfigured && <Badge>SLA belum dikonfigurasi</Badge>}
      </div>
      <p className="sr-only" role="status">{d ? `Dashboard ${PERIOD_LABELS[d.period.key]} dimuat.` : ''}</p>
      {state.status === 'loading' && <DashboardSkeleton />}
      {state.status === 'error' && (
        <DashboardError error={state.error} onRetry={() => setAttempt((n) => n + 1)} />
      )}
      {d && <DashboardBody d={d} uat={uat} />}
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

function DashboardError({ error, onRetry }: { error: unknown; onRetry: () => void }) {
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

function DashboardBody({ d, uat }: { d: Dashboard; uat: boolean }) {
  const ws = d.workspaceId;
  const t = d.totals;
  const periodText = PERIOD_LABELS[d.period.key];
  const drill = (f: Dashboard['drilldown']['active']) => drilldownHash(ws, f);
  const resolvedHint = t.resolvedTicketsInPeriod !== t.resolvedInPeriod
    ? `${periodText} · ${num(t.resolvedTicketsInPeriod)} tiket`
    : periodText;
  const kpis = [
    { label: 'Tiket aktif', value: t.active, hint: 'Seluruh status nonterminal', href: drill(d.drilldown.active) },
    { label: 'Belum ditugaskan', value: t.unassigned, hint: 'Butuh penanggung jawab', href: drill(d.drilldown.unassigned) },
    { label: 'Menunggu pelanggan', value: t.pendingCustomer, hint: 'Tindak lanjut pelanggan', href: drill(d.drilldown.pendingCustomer) },
    { label: 'Terselesaikan', value: t.resolvedInPeriod, hint: resolvedHint, href: drill(d.drilldown.resolvedInPeriod) },
  ];

  return (
    <>
      <div className="kpis">
        {kpis.map((k) => (
          <a className="kpi" key={k.label} href={k.href}>
            <span className="muted">{k.label}</span>
            <strong>{num(k.value)}</strong>
            <small>{k.hint} <span aria-hidden="true">↗</span></small>
          </a>
        ))}
      </div>
      {t.tickets === 0 ? (
        <EmptyState title="Belum ada tiket"
          body={`Belum ada tiket di ${d.workspaceCode}. Ringkasan muncul setelah tiket pertama diterima.`} />
      ) : (
        <>
          <div className="grid2">
            <section className="card" aria-labelledby="dash-status">
              <h2 id="dash-status">Di mana tiket sedang berada?</h2>
              <p className="muted tiny">Distribusi status · seluruh {num(t.tickets)} tiket {d.workspaceCode}</p>
              <BarList label="Distribusi tiket menurut status" total={t.tickets}
                items={d.statusDistribution.map((s) => ({ key: s.status.code, label: s.status.label, value: s.count }))} />
              <details>
                <summary>Tampilkan angka</summary>
                <ul className="plain">
                  {d.statusDistribution.map((s) => (
                    <li key={s.status.code}><a href={drill(s.filter)}>{s.status.label}: {num(s.count)}</a></li>
                  ))}
                </ul>
              </details>
            </section>
            <section className="card" aria-labelledby="dash-pic">
              <h2 id="dash-pic">Beban PIC</h2>
              <p className="muted tiny">Tiket aktif, bukan seluruh histori</p>
              <ul className="plain">
                {d.picLoad.map((p) => {
                  const name = p.displayName ?? 'Belum ditugaskan';
                  return (
                    <li className="row between activity" key={p.userId ?? 'unassigned'}>
                      <a className="pic" href={drill(p.filter)}>
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
              {periodText} · {rangeLabel(d.period.startDate, d.period.endDate)} · tiket diterima dibandingkan tiket terselesaikan
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
              Bukan penjumlahan dua kelompok. Terselesaikan dihitung dari peristiwa penyelesaian; pembukaan ulang tidak
              menghapusnya.
            </p>
            <details>
              <summary>Tampilkan angka</summary>
              <TrendTable d={d} />
            </details>
          </section>

          <div className="grid2 spaced">
            <section className="card" aria-labelledby="dash-channel">
              <h2 id="dash-channel">Dari kanal mana tiket masuk?</h2>
              <p className="tiny muted">Tiket diterima · {periodText}</p>
              {t.receivedInPeriod === 0 ? (
                <p className="muted">Tidak ada tiket diterima pada periode ini.</p>
              ) : (
                <ul className="plain">
                  {d.channels.map((c) => (
                    <li className="row between activity" key={c.channelId ?? 'none'}>
                      <span>{c.label ?? 'Tanpa kanal'}</span><b>{num(c.count)}</b>
                    </li>
                  ))}
                </ul>
              )}
            </section>
            <section className="card" aria-labelledby="dash-age">
              <h2 id="dash-age">Umur tiket aktif</h2>
              <p className="tiny muted">Usia sejak diterima · bukan pelanggaran SLA</p>
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
              <a className="btn btn-ghost" href={drill(d.drilldown.attention)}>Semua tiket →</a>
            </div>
            <p className="tiny muted">
              Tiket aktif prioritas Mendesak/Tinggi · {num(d.attention.items.length)} dari {num(d.attention.total)} ·
              Mendesak dulu, lalu yang paling lama diterima
            </p>
            <DataTable caption="Tiket perlu perhatian" columns={ATTENTION_COLUMNS} rows={d.attention.items}
              rowKey={(r) => r.id}
              empty={{ title: 'Tidak ada tiket mendesak', body: 'Tidak ada tiket aktif berprioritas Mendesak atau Tinggi.' }} />
          </section>
        </>
      )}
      <p className="tiny muted source">
        Sumber: API ITSM, agregat server seluruh {d.workspaceCode} · per {instantLabel(d.asOf, d.period.timezone)}{' '}
        {tzLabel(d.period.timezone)}{uat ? ' · data dummy UAT' : ''}
      </p>
    </>
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
  { key: 'age', header: 'Umur', cell: (r) => ageLabel(r.ageSeconds) },
];
