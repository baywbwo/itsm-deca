import { ApiError, rateLimitText } from '../api/client';

/**
 * CP4c (US-1.2, A-9) helpers for the MFA screens. Codes and seeds pass through here only as
 * arguments; nothing is stored or logged.
 */

/** TM-02 d: the fifth failure in a row locks the authenticator for 15 minutes. */
export const MFA_LOCK_AFTER = 5;
export const MFA_LOCK_MINUTES = 15;

/** Seed, issuer and account read from an `otpauth://totp/…` URI (no QR library in R1). */
export interface OtpauthParts { secret: string; issuer: string | null; account: string | null }

export function parseOtpauth(uri: string): OtpauthParts | null {
  let url: URL;
  try { url = new URL(uri); } catch { return null; }
  if (url.protocol !== 'otpauth:') return null;
  const secret = url.searchParams.get('secret');
  if (!secret || !/^[A-Z2-7]+=*$/i.test(secret)) return null;
  const label = decodeURIComponent(url.pathname.replace(/^\/+totp\/|^totp\//, '').replace(/^\/+/, ''));
  const [labelIssuer, account] = label.includes(':') ? label.split(/:(.*)/s) : [null, label || null];
  return { secret: secret.toUpperCase(), issuer: url.searchParams.get('issuer') ?? labelIssuer ?? null, account: account ?? null };
}

/** `ABCDEFGH…` → `ABCD EFGH …` so a person can type it into an authenticator. */
export const groupSecret = (secret: string) => secret.replace(/=+$/, '').match(/.{1,4}/g)?.join(' ') ?? '';

/** Six digits (spaces removed) — what the enrolment and login code fields accept. */
export const normalizeTotp = (input: string) => input.replace(/\s+/g, '');
export const isTotp = (input: string) => /^\d{6}$/.test(normalizeTotp(input));

/** A recovery code is 16 base32 characters, grouped `XXXX-XXXX-XXXX-XXXX`. */
export const isRecoveryCode = (input: string) => /^[A-Z2-7]{16}$/.test(input.replace(/[\s-]/g, '').toUpperCase());

/**
 * Step-up dialog refusal → text. `failures` counts consecutive mfa_invalid answers in this dialog:
 * the server answers a locked authenticator with the same mfa_invalid, so after the fifth the
 * dialog says the lock may be on and that a recovery code still works.
 */
export function stepUpErrorText(e: unknown, failures: number): string {
  if (e instanceof ApiError) {
    if (e.code === 'mfa_invalid') {
      return failures >= MFA_LOCK_AFTER
        ? `Kode ditolak ${failures} kali berturut-turut. Autentikator mungkin terkunci ${MFA_LOCK_MINUTES} menit; gunakan recovery code atau coba lagi nanti.`
        : 'Kode tidak valid atau sudah dipakai. Periksa kode terbaru di aplikasi autentikator.';
    }
    if (e.code === 'mfa_not_enrolled') return 'Akun ini belum memakai autentikator. Aktifkan MFA di menu akun → Keamanan akun.';
    if (e.status === 429) return rateLimitText(e);
    if (e.code === 'network') return 'Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.';
  }
  return 'Verifikasi belum berhasil. Coba lagi.';
}

/** Second login step refusal → text; `restart` sends the person back to the password step. */
export function loginMfaError(e: unknown): { text: string; restart: boolean } {
  if (e instanceof ApiError) {
    if (e.status === 410) return { text: 'Waktu verifikasi habis. Masukkan nama pengguna dan kata sandi lagi.', restart: true };
    if (e.code === 'mfa_invalid') {
      return { text: `Kode tidak valid atau sudah dipakai. Setelah ${MFA_LOCK_AFTER} kali salah, autentikator terkunci ${MFA_LOCK_MINUTES} menit; recovery code tetap dapat dipakai.`, restart: false };
    }
    if (e.status === 429) return { text: rateLimitText(e), restart: false };
    if (e.code === 'network') return { text: 'Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.', restart: false };
  }
  return { text: 'Verifikasi belum berhasil. Coba lagi.', restart: false };
}

/** Enrolment confirm/start refusal → text; `restart` asks for a new seed. */
export function enrolError(e: unknown): { text: string; restart: boolean; enrolled: boolean } {
  const r = (text: string, restart = false, enrolled = false) => ({ text, restart, enrolled });
  if (e instanceof ApiError) {
    if (e.status === 422) return r('Kode tidak cocok. Pastikan jam perangkat tepat lalu masukkan kode terbaru.');
    if (e.status === 410) return r('Waktu pendaftaran habis (10 menit). Mulai lagi untuk mendapat kunci baru.', true);
    if (e.code === 'mfa_already_enrolled') return r('Autentikator sudah aktif untuk akun ini.', false, true);
    if (e.status === 404) return r('Pendaftaran tidak ditemukan. Mulai lagi.', true);
    if (e.status === 429) return r(rateLimitText(e));
    if (e.status === 503) return r('MFA belum dapat dipakai di server ini. Hubungi administrator.');
    if (e.code === 'network') return r('Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.');
  }
  return r('Pendaftaran belum berhasil. Coba lagi.');
}
