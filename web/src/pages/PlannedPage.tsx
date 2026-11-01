import type { Workspace } from '../api/types';
import type { PageDef } from '../app/routes';
import { EmptyState } from '../components/States';
import { PageHeader } from '../components/PageHeader';

/** Honest placeholder: the route and its permission gate exist, the screen does not yet. */
export function PlannedPage({ page, workspace }: { page: PageDef; workspace: Workspace }) {
  return (
    <>
      <PageHeader eyebrow={workspace.code} title={page.label}
        description={`Layar ini belum dibangun pada kandidat D04; target pembangunan ${page.milestone}.`} />
      <EmptyState title="Belum tersedia"
        body={`Navigasi ini muncul karena izin server Anda di ${workspace.code}. Fungsinya belum ada, jadi tidak ada data yang ditampilkan.`} />
    </>
  );
}
