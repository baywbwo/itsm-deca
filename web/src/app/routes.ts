import type { PermissionCode } from '../api/types';

export type PageId = 'dashboard' | 'tickets' | 'reports' | 'settings' | 'integrations' | 'components';

export interface PageDef {
  id: PageId;
  label: string;
  /** Visible when the server grants ANY of these here; empty = no domain permission needed. */
  anyOf: readonly PermissionCode[];
  /** Only in UAT mode (design/engineering aid, not a product screen). */
  uatOnly?: boolean;
  /** Delivery step that builds the real screen. */
  milestone: string;
}

export const PAGES: readonly PageDef[] = [
  { id: 'dashboard', label: 'Dashboard', anyOf: ['ticket.read'], milestone: 'D05' },
  { id: 'tickets', label: 'Tiket', anyOf: ['ticket.read'], milestone: 'D06–D08' },
  { id: 'reports', label: 'Laporan', anyOf: ['report.view'], milestone: 'D10' },
  {
    id: 'settings',
    label: 'Pengaturan',
    anyOf: ['admin.settings', 'admin.members', 'member.read', 'catalog.read'],
    milestone: 'D09',
  },
  {
    id: 'integrations',
    label: 'Integrasi',
    anyOf: ['integration.manage', 'integration.credentials', 'integration.recovery'],
    milestone: 'D09–D10',
  },
  { id: 'components', label: 'Komponen', anyOf: [], uatOnly: true, milestone: 'D04' },
];

export const pageDef = (id: PageId): PageDef => PAGES.find((p) => p.id === id) ?? PAGES[0]!;

export function allowedPages(permissions: readonly PermissionCode[], uat: boolean): PageDef[] {
  return PAGES.filter(
    (p) => (!p.uatOnly || uat) && (p.anyOf.length === 0 || p.anyOf.some((c) => permissions.includes(c))),
  );
}

export interface Route {
  workspaceId: string | null;
  page: PageId;
}

const PAGE_IDS = new Set<string>(PAGES.map((p) => p.id));

/** `#/w/<workspaceId>/<page>`; anything else falls back to no workspace + dashboard. */
export function parseHash(hash: string): Route {
  const parts = hash.replace(/^#\/?/, '').split('/').filter(Boolean);
  if (parts[0] !== 'w' || !parts[1]) return { workspaceId: null, page: 'dashboard' };
  const page = parts[2] && PAGE_IDS.has(parts[2]) ? (parts[2] as PageId) : 'dashboard';
  return { workspaceId: decodeURIComponent(parts[1]), page };
}

export const toHash = (r: { workspaceId: string; page: PageId }) =>
  `#/w/${encodeURIComponent(r.workspaceId)}/${r.page}`;
