import type { ScopeData } from '../app/hooks';
import { roleLabel } from '../app/labels';
import { Badge } from '../components/Badge';
import { PageHeader } from '../components/PageHeader';

/** D04 shows only the real workspace scope; operational widgets arrive in D05. */
export function DashboardPage({ data }: { data: ScopeData }) {
  const { workspace: w, permissions: p } = data;
  return (
    <>
      <PageHeader eyebrow={w.code} title="Dashboard"
        description="Ringkasan operasional workspace dibangun di D05. Halaman ini baru menampilkan scope akses dari server." />
      <section className="card" aria-labelledby="scope-title">
        <h2 id="scope-title">Scope workspace aktif</h2>
        <dl className="props">
          <dt>Workspace</dt><dd>{w.code} · {w.name}</dd>
          <dt>Zona waktu</dt><dd>{w.timezone}</dd>
          <dt>Peran Anda</dt><dd>{roleLabel(p)}</dd>
          <dt>Izin efektif</dt>
          <dd>
            <ul className="row" aria-label="Izin efektif" style={{ listStyle: 'none', padding: 0, margin: 0 }}>
              {p.permissions.map((c) => <li key={c}><Badge tone="neutral">{c}</Badge></li>)}
            </ul>
          </dd>
          <dt>Grant eksplisit</dt>
          <dd>{p.grants.length ? p.grants.join(', ') : 'Tidak ada'}</dd>
        </dl>
        <p className="tiny muted">Izin di sini hanya petunjuk tampilan; server memeriksa ulang setiap permintaan.</p>
      </section>
    </>
  );
}
