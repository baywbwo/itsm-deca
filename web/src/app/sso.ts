import type { SsoProvider, SsoRoleMapping, SsoSettingsRequest } from '../api/types';

// --- Login entry (CP5c) ------------------------------------------------------

/** The company code typed on the login, remembered on this device only (not a secret, no list). */
export const SSO_COMPANY_KEY = 'itsm.sso.company';

/** Same rule as the server's `/auth/oidc/{code}/start`: 2–10 letters or digits, upper-cased. */
export function ssoCompanyCode(raw: string): string | null {
  const code = raw.trim().toUpperCase();
  return /^[A-Z0-9]{2,10}$/.test(code) ? code : null;
}

export const ssoStartPath = (code: string) => `/api/v1/auth/oidc/${encodeURIComponent(code)}/start`;

const LOGIN_TEXT: Record<string, string> = {
  not_configured: 'Login SSO belum diaktifkan untuk kode perusahaan ini. Periksa kodenya atau hubungi admin platform.',
  unavailable: 'Login SSO sedang tidak tersedia di server ini. Hubungi admin platform.',
  idp_unavailable: 'Penyedia login kantor tidak dapat dihubungi. Coba lagi beberapa saat lagi.',
  idp_error: 'Penyedia login kantor menolak atau membatalkan proses masuk. Coba lagi.',
  denied: 'Identitas dari penyedia login tidak dapat diverifikasi. Tidak ada sesi yang dibuat; coba lagi.',
  expired: 'Proses masuk kedaluwarsa atau sudah dipakai. Mulai lagi dari tombol Masuk dengan SSO.',
  inactive: 'Akun Anda nonaktif. Hubungi admin platform.',
  not_linked: 'Akun kantor Anda belum terhubung ke ITSM. Minta admin platform menghubungkan akun Anda.',
  email_in_use: 'Email akun kantor Anda sudah dipakai akun ITSM lain. Minta admin platform menghubungkan akun tersebut.',
};

/** `?sso=<code>` from the OIDC callback → what the login says. Unknown codes read as a generic failure. */
export function ssoLoginNotice(code: string | null): { title: string; text: string } | null {
  if (!code) return null;
  return { title: 'Masuk dengan SSO gagal', text: LOGIN_TEXT[code] ?? 'Proses masuk tidak selesai. Coba lagi atau hubungi admin platform.' };
}

/** Reads `?sso=` once and removes it from the address bar so a reload does not repeat the message. */
export function takeSsoResult(loc: Pick<Location, 'search' | 'pathname' | 'hash'> = location,
  replace: (url: string) => void = (url) => history.replaceState(history.state, '', url)): string | null {
  const q = new URLSearchParams(loc.search);
  const code = q.get('sso');
  if (code === null) return null;
  q.delete('sso');
  const rest = q.toString();
  replace(`${loc.pathname}${rest ? `?${rest}` : ''}${loc.hash}`);
  return code.slice(0, 40);
}

// --- Admin page (CP5c slice 4) -----------------------------------------------

export interface SsoDraft {
  displayName: string;
  issuer: string;
  clientId: string;
  /** One per line (or comma separated). */
  emailDomains: string;
  extraHosts: string;
  groupClaim: string;
  roleMapping: SsoRoleMapping[];
  syncRoles: boolean;
}

export const emptySsoDraft = (): SsoDraft => ({
  displayName: '', issuer: '', clientId: '', emailDomains: '', extraHosts: '', groupClaim: 'groups', roleMapping: [], syncRoles: false,
});

export const ssoDraftOf = (p: SsoProvider): SsoDraft => ({
  displayName: p.displayName, issuer: p.issuer, clientId: p.clientId,
  emailDomains: p.emailDomains.join('\n'), extraHosts: p.extraHosts.join('\n'),
  groupClaim: p.groupClaim, roleMapping: p.roleMapping.map((m) => ({ ...m })), syncRoles: p.syncRoles,
});

/** Lines or commas → trimmed, lower-cased, de-duplicated entries (the server normalises the same way). */
export function splitList(text: string): string[] {
  const out: string[] = [];
  for (const v of text.split(/[\n,]/)) {
    const s = v.trim().toLowerCase();
    if (s && !out.includes(s)) out.push(s);
  }
  return out;
}

/** Request body; an empty mapping row is dropped, `version` null creates the provider. */
export function ssoBody(d: SsoDraft, version: number | null): SsoSettingsRequest {
  return {
    displayName: d.displayName.trim(), issuer: d.issuer.trim(), clientId: d.clientId.trim(),
    emailDomains: splitList(d.emailDomains), extraHosts: splitList(d.extraHosts),
    groupClaim: d.groupClaim.trim(),
    roleMapping: d.roleMapping.map((m) => ({ group: m.group.trim(), role: m.role })).filter((m) => m.group !== ''),
    syncRoles: d.syncRoles, version,
  };
}

/** Changing the issuer, client id or extra hosts switches the provider off and needs a new test. */
export function ssoMoves(p: SsoProvider, d: SsoDraft): boolean {
  const same = (a: readonly string[], b: readonly string[]) => a.length === b.length && a.every((x) => b.includes(x));
  return p.issuer !== d.issuer.trim() || p.clientId !== d.clientId.trim() || !same(p.extraHosts, splitList(d.extraHosts));
}

export type SsoState = 'none' | 'draft' | 'ready' | 'on';

/** none = no provider; draft = not yet testable/tested; ready = may be switched on; on = enabled. */
export function ssoState(p: SsoProvider | null, keyring: boolean): SsoState {
  if (!p) return 'none';
  if (p.enabled) return 'on';
  return keyring && p.secretSet && p.lastTestOk === true ? 'ready' : 'draft';
}

/** Why "Aktifkan" is not offered yet, in the order the admin has to fix it. */
export function ssoEnableBlocker(p: SsoProvider, keyring: boolean): string | null {
  if (!keyring) return 'Kunci enkripsi server (ITSM_KEYRING) belum dipasang.';
  if (!p.secretSet) return 'Simpan client secret dahulu.';
  if (p.lastTestOk !== true) return 'Uji koneksi sampai berhasil dahulu.';
  return null;
}

export const SSO_FIELD_TEXT: Record<string, string> = {
  displayName: 'Nama tampilan wajib diisi, maksimal 80 karakter.',
  issuer: 'Issuer harus alamat https tanpa query atau fragmen (mis. https://login.microsoftonline.com/<tenant>/v2.0).',
  clientId: 'Client ID wajib diisi, maksimal 255 karakter.',
  emailDomains: 'Domain email tidak valid atau lebih dari 20.',
  extraHosts: 'Host tambahan tidak valid atau lebih dari 10.',
  groupClaim: 'Nama klaim grup 1–100 karakter: huruf, angka, _ . : / -.',
  roleMapping: 'Pemetaan grup tidak valid: nama grup 1–255 karakter, tanpa duplikat, maksimal 50.',
  secret: 'Client secret 1–4000 karakter tanpa baris baru.',
  userId: 'Pengguna tidak ditemukan. Pilih dari hasil pencarian.',
  subject: 'Subject wajib diisi, maksimal 255 karakter.',
  q: 'Kata kunci maksimal 100 karakter.',
};

/** Stage/code of a failed discovery test, in words (codes come from the server, never IdP text). */
export function ssoTestText(r: { ok: boolean; stage: string | null; code: string | null }): string {
  if (r.ok) return 'Discovery dan kunci penandatangan (JWKS) berhasil dibaca.';
  const stage = r.stage === 'policy' ? 'Alamat ditolak kebijakan host'
    : r.stage === 'jwks' ? 'Kunci penandatangan (JWKS) gagal dibaca'
      : 'Dokumen discovery gagal dibaca';
  return `${stage}${r.code ? ` (${r.code})` : ''}. Periksa issuer, host tambahan dan akses jaringan server.`;
}

// --- Signed in without a company (CP5c) --------------------------------------

/**
 * A signed-in account with no company yet (SSO account whose groups map to no role, or a
 * membership removed): it waits for an administrator instead of an empty shell. Platform admins
 * and the UAT Superadmin keep the shell (they administer from it).
 */
export const awaitingAccess = (s: { platformAdmin: boolean; uatSuperadmin: boolean }, workspaces: readonly unknown[]) =>
  workspaces.length === 0 && !s.platformAdmin && !s.uatSuperadmin;

// --- Remembered company code (login) -----------------------------------------

export function rememberedSsoCompany(store: Pick<Storage, 'getItem'> | null = safeStorage()): string {
  try {
    return ssoCompanyCode(store?.getItem(SSO_COMPANY_KEY) ?? '') ?? '';
  } catch {
    return '';
  }
}

export function rememberSsoCompany(code: string, store: Pick<Storage, 'setItem'> | null = safeStorage()) {
  try {
    store?.setItem(SSO_COMPANY_KEY, code);
  } catch {
    // Private mode or storage off: the code is simply not remembered.
  }
}

function safeStorage(): Storage | null {
  try {
    return typeof localStorage === 'undefined' ? null : localStorage;
  } catch {
    return null;
  }
}
