import { ApiError, rateLimitText } from '../api/client';

/** CP3a: shown on the login after a session ended on the server (401 in the middle of work). */
export const SESSION_ENDED_TITLE = 'Sesi berakhir';
export const SESSION_ENDED_TEXT = 'Sesi Anda tidak lagi aktif. Masuk kembali untuk melanjutkan.';
export const LOGGED_OUT_ALL_TEXT = 'Anda telah keluar dari semua perangkat. Masuk kembali untuk melanjutkan.';

/** Same rule as the server (US-1.4): 12 characters, counted as characters, not bytes. */
export const MIN_PASSWORD_LENGTH = 12;

export interface PasswordChangeInput {
  currentPassword: string;
  newPassword: string;
  confirmPassword: string;
  /** Username typed at login in this tab; null after a reload (the server still checks it). */
  username: string | null;
}
export type PasswordField = 'currentPassword' | 'newPassword' | 'confirmPassword';
export type PasswordErrors = Partial<Record<PasswordField, string>>;

const NEW_PASSWORD_RULES = 'Kata sandi baru minimal 12 karakter, tidak sama dengan nama pengguna, dan berbeda dari kata sandi saat ini.';

/** Client-side checks before the request; the server re-checks every rule. */
export function validatePasswordChange(v: PasswordChangeInput): PasswordErrors {
  const errors: PasswordErrors = {};
  if (!v.currentPassword) errors.currentPassword = 'Isi kata sandi saat ini.';
  if ([...v.newPassword].length < MIN_PASSWORD_LENGTH) {
    errors.newPassword = `Kata sandi baru minimal ${MIN_PASSWORD_LENGTH} karakter.`;
  } else if (v.username && v.newPassword.trim().toLowerCase() === v.username.trim().toLowerCase()) {
    errors.newPassword = 'Kata sandi baru tidak boleh sama dengan nama pengguna.';
  } else if (v.newPassword === v.currentPassword) {
    errors.newPassword = 'Kata sandi baru harus berbeda dari kata sandi saat ini.';
  }
  if (!errors.newPassword && v.confirmPassword !== v.newPassword) {
    errors.confirmPassword = 'Konfirmasi tidak sama dengan kata sandi baru.';
  }
  return errors;
}

/**
 * Server answer → where to show it. 422 names one field; the server does not say which rule of
 * the new password failed, so the message lists all of them.
 */
export function passwordChangeError(e: unknown): { fields: PasswordErrors; form: string | null } {
  if (e instanceof ApiError) {
    if (e.status === 422 && e.field === 'currentPassword') {
      return { fields: { currentPassword: 'Kata sandi saat ini salah.' }, form: null };
    }
    if (e.status === 422) return { fields: { newPassword: NEW_PASSWORD_RULES }, form: null };
    if (e.status === 429) return { fields: {}, form: rateLimitText(e) };
    if (e.code === 'network') return { fields: {}, form: 'Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.' };
    const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
    return { fields: {}, form: `Kata sandi belum dapat diganti. Coba lagi.${rid}` };
  }
  return { fields: {}, form: 'Kata sandi belum dapat diganti. Coba lagi.' };
}
