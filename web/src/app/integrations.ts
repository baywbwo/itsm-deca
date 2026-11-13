import type { IntegrationWebhook, Note } from '../api/types';

/** Fields a stored provider credential is bound to (mirrors the server's CP5B-01 rule). */
export interface CredentialTarget {
  baseUrl: string;
  email: string;
  appId: string;
}

const LARK_DEFAULT_BASE = 'https://open.larksuite.com';

function base(provider: 'jira' | 'lark', raw: string): string {
  const b = raw.trim().replace(/\/+$/, '').toLowerCase();
  return provider === 'lark' && b === '' ? LARK_DEFAULT_BASE : b;
}

/**
 * True when saving `draft` would point the stored credential at another address or account,
 * so the server deletes it (Jira: address or email; Lark: address or App ID; an empty Lark
 * address is the international default).
 */
export function credentialRetargeted(provider: 'jira' | 'lark', saved: CredentialTarget, draft: CredentialTarget): boolean {
  if (base(provider, draft.baseUrl) !== base(provider, saved.baseUrl)) return true;
  return provider === 'jira' ? draft.email.trim() !== saved.email : draft.appId.trim() !== saved.appId;
}

/** Timeline byline for a follow-up entry: an officer note, or an entry written by the Jira webhook (CP6a). */
export function noteByline(n: Note): { name: string; source: string | null } {
  if (n.kind === 'note') return { name: n.author?.label ?? 'Petugas', source: null };
  return { name: n.externalAuthor ?? 'Pengguna Jira', source: NOTE_SOURCE[n.kind] };
}
const NOTE_SOURCE: Record<Exclude<Note['kind'], 'note'>, string> = {
  jira_comment: 'Komentar Jira',
  jira_status: 'Status Jira',
  jira_assignee: 'Penerima tugas Jira',
};

/**
 * Address Jira must POST to. The server knows it only when ITSM_PUBLIC_URL is set; otherwise the
 * browser's own origin is the best guess (the admin must check it is reachable from Jira).
 */
export function webhookAddress(w: Pick<IntegrationWebhook, 'path' | 'url'>, origin: string): { url: string; guessed: boolean } {
  return w.url ? { url: w.url, guessed: false } : { url: `${origin.replace(/\/+$/, '')}${w.path}`, guessed: true };
}
