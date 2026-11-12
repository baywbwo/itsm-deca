import type { WorkspacePermissions } from '../api/types';

export function roleLabel(p: Pick<WorkspacePermissions, 'role' | 'uatSuperadmin'>): string {
  if (p.role === 'lead_cs') return 'Lead CS';
  if (p.role === 'cs') return 'CS';
  return p.uatSuperadmin ? 'Superadmin (tanpa keanggotaan)' : 'Tanpa peran';
}

export function initials(name: string): string {
  const parts = name.replace(/\(.*?\)/g, '').trim().split(/\s+/).filter(Boolean);
  return ((parts[0]?.[0] ?? '') + (parts[1]?.[0] ?? '')).toUpperCase() || '?';
}
