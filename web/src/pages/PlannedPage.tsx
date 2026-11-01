import type { Workspace } from '../api/types';
import type { PageDef } from '../app/routes';
import { EmptyState } from '../components/States';
import { PageHeader } from '../components/PageHeader';

const FILTER_LABELS: Record<string, string> = {
  status: 'Status',
  owner: 'PIC',
  priority: 'Prioritas',
  receivedFrom: 'Diterima sejak',
  receivedBefore: 'Diterima sebelum',
  resolvedFrom: 'Diselesaikan sejak',
  resolvedBefore: 'Diselesaikan sebelum',
};

/** Honest placeholder: the route and its permission gate exist, the screen does not yet. */
export function PlannedPage({ page, workspace, query = '' }: { page: PageDef; workspace: Workspace; query?: string }) {
  const filters = [...new URLSearchParams(query)].filter(([k]) => k in FILTER_LABELS);
  return (
    <>
      <PageHeader eyebrow={workspace.code} title={page.label}
        description={`Layar ini belum dibangun pada kandidat ini; target pembangunan ${page.milestone}.`} />
      {filters.length > 0 && (
        <section className="card spaced-bottom" aria-labelledby="carried-filters">
          <h2 id="carried-filters">Filter dari Dashboard</h2>
          <p className="tiny muted">
            Parameter drilldown sudah dibawa dan dibatasi ke {workspace.code}, tetapi daftar tiket (D06) belum dibangun,
            jadi belum ada tiket yang ditampilkan.
          </p>
          <dl className="props">
            {filters.map(([k, v]) => (
              <div key={k} className="props-row">
                <dt>{FILTER_LABELS[k]}</dt>
                <dd><code>{v}</code></dd>
              </div>
            ))}
          </dl>
        </section>
      )}
      <EmptyState title="Belum tersedia"
        body={`Navigasi ini muncul karena izin server Anda di ${workspace.code}. Fungsinya belum ada, jadi tidak ada data yang ditampilkan.`} />
    </>
  );
}
