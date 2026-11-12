import { ApiError, isSessionReloadNeeded, rateLimitText, sessionReloadText } from '../api/client';

/** CP3b user administration actions, for the words of a refusal. */
export type UserAction = 'create' | 'reset' | 'deactivate';

export type UserField = 'username' | 'displayName' | 'email';

/** One refused user-administration write: a field to mark (create form) or a message. */
export interface UserWriteError {
  field: UserField | null;
  text: string;
  /** The list no longer matches the server (404, already inactive): reload it. */
  reload: boolean;
}

const FIELD_TEXT: Record<UserField, string> = {
  username: 'Nama pengguna 3–254 karakter tanpa spasi.',
  displayName: 'Nama tampilan wajib diisi, maksimal 200 karakter.',
  email: 'Format email tidak valid.',
};

const isField = (f: string | null): f is UserField => f === 'username' || f === 'displayName' || f === 'email';

/** Server refusal → what the admin reads. Nothing here ever carries a password. */
export function userWriteError(e: unknown, action: UserAction): UserWriteError {
  const rid = e instanceof ApiError && e.requestId ? ` (ID permintaan ${e.requestId})` : '';
  const msg = (text: string, reload = false): UserWriteError => ({ field: null, text: `${text}${rid}`, reload });
  if (isSessionReloadNeeded(e)) return msg(sessionReloadText(null));
  if (!(e instanceof ApiError)) return msg('Terjadi kesalahan tak terduga. Tidak ada yang berubah.');
  switch (e.code) {
    case 'username_taken':
      return { field: 'username', text: 'Nama pengguna sudah dipakai akun lain.', reload: false };
    case 'email_taken':
      return { field: 'email', text: 'Email sudah dipakai akun lain.', reload: false };
    case 'last_admin':
      return msg('Tidak dapat menonaktifkan admin platform terakhir. Harus tetap ada minimal satu admin platform aktif.');
    case 'user_inactive':
      return msg(action === 'reset'
        ? 'Pengguna ini sudah nonaktif; kata sandinya tidak dapat direset. Daftar dimuat ulang.'
        : 'Pengguna ini sudah nonaktif. Daftar dimuat ulang.', true);
    default:
      break;
  }
  if (e.status === 422 && isField(e.field)) return { field: e.field, text: FIELD_TEXT[e.field], reload: false };
  if (e.status === 429) return msg(`${rateLimitText(e)} Tidak ada yang berubah.`);
  if (e.status === 403) return msg('Hanya admin platform yang dapat mengelola pengguna. Tidak ada yang berubah.');
  if (e.status === 404) return msg('Pengguna tidak ditemukan atau tidak memiliki kata sandi lokal. Daftar dimuat ulang.', true);
  if (e.code === 'network') return msg('Server tidak dapat dihubungi. Hasilnya belum diketahui; muat ulang daftar sebelum mencoba lagi.', true);
  return msg(`${e.message} Tidak ada yang berubah.`);
}

/** Active filter of the directory, as the API takes it. */
export type ActiveFilter = 'all' | 'true' | 'false';
export const ACTIVE_FILTERS: readonly { value: ActiveFilter; label: string }[] = [
  { value: 'all', label: 'Semua status' },
  { value: 'true', label: 'Aktif' },
  { value: 'false', label: 'Nonaktif' },
];
