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
