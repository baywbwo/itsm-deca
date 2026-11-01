import { useEffect, useRef, useState, type KeyboardEvent, type ReactNode } from 'react';
import { ApiError } from '../api/client';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';

/** One failed D09 write, in words: what happened and that nothing changed. */
export interface WriteError {
  title: string;
  body: string;
  /** 409/404: the record moved on; offer a reload and keep the input. */
  conflict: boolean;
  /** 422 with a field: the input to mark. */
  field: string | null;
}

export function writeError(e: unknown, what: string): WriteError {
  if (e instanceof ApiError) {
    const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
    switch (e.status) {
      case 409:
        return {
          title: 'Data sudah diubah pihak lain',
          body: `${what} tidak disimpan. Isian Anda tetap di sini; muat ulang data lalu terapkan lagi.${rid}`,
          conflict: true, field: null,
        };
      case 403:
        return { title: 'Tidak diizinkan', body: `Hak Anda di workspace ini tidak mencakup perubahan ini. Tidak ada yang berubah.${rid}`, conflict: false, field: null };
      case 404:
        return { title: 'Data tidak ditemukan', body: `Data ini tidak ada atau tidak dapat Anda akses. Muat ulang daftar.${rid}`, conflict: true, field: null };
      case 422:
        return { title: 'Ditolak aturan', body: `${e.message} Tidak ada yang berubah.${rid}`, conflict: false, field: e.field };
      case 400:
        return { title: 'Isian tidak valid', body: `Periksa isian lalu coba lagi. Tidak ada yang berubah.${rid}`, conflict: false, field: null };
      default:
        return { title: 'Belum tersimpan', body: `${e.message} ${what} belum berubah; coba lagi.${rid}`, conflict: false, field: null };
    }
  }
  return { title: 'Belum tersimpan', body: `Terjadi kesalahan tak terduga. ${what} belum berubah.`, conflict: false, field: null };
}

/** Focused alert for a failed write; a conflict offers a reload instead of guessing. */
export function WriteAlert({ error, onReload }: { error: WriteError; onReload?: () => void }) {
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => { ref.current?.focus(); }, [error]);
  return (
    <div ref={ref} tabIndex={-1} className="state-error spaced-bottom" role="alert">
      <h3>{error.title}</h3>
      <p>{error.body}</p>
      {error.conflict && onReload && <Button onClick={onReload}>Muat ulang data</Button>}
    </div>
  );
}

/** C31 high-impact confirmation: Cancel has the first focus, the impact is spelled out. */
export function ConfirmDialog({ title, impact, confirmLabel, onConfirm, onClose, children }: {
  title: string;
  impact: string;
  confirmLabel: string;
  onConfirm: () => Promise<void>;
  onClose: () => void;
  children?: ReactNode;
}) {
  const [busy, setBusy] = useState(false);
  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title={title} initialFocus="[data-cancel]"
      footer={(
        <>
          <Button data-cancel onClick={onClose} disabled={busy}>Batal</Button>
          <Button variant="primary" aria-busy={busy} disabled={busy}
            onClick={() => { setBusy(true); void onConfirm().finally(() => setBusy(false)); }}>
            {busy ? 'Memproses…' : confirmLabel}
          </Button>
        </>
      )}>
      <p>{impact}</p>
      {children}
    </Dialog>
  );
}

/** C04 tablist with roving focus (arrow keys, Home/End). */
export function Tabs<T extends string>({ tabs, value, onChange, label, panelId }: {
  tabs: readonly { id: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
  label: string;
  panelId: string;
}) {
  const refs = useRef<(HTMLButtonElement | null)[]>([]);
  const onKey = (e: KeyboardEvent, i: number) => {
    const delta = e.key === 'ArrowRight' ? 1 : e.key === 'ArrowLeft' ? -1 : 0;
    const next = delta ? (i + delta + tabs.length) % tabs.length : e.key === 'Home' ? 0 : e.key === 'End' ? tabs.length - 1 : -1;
    if (next < 0) return;
    e.preventDefault();
    refs.current[next]?.focus();
    onChange(tabs[next]!.id);
  };
  return (
    <div className="tabs detail-tabs spaced-bottom" role="tablist" aria-label={label}>
      {tabs.map((t, i) => (
        <button key={t.id} ref={(el) => { refs.current[i] = el; }} type="button" role="tab" id={`${panelId}-tab-${t.id}`}
          aria-selected={value === t.id} aria-controls={panelId} tabIndex={value === t.id ? 0 : -1}
          className={value === t.id ? 'active' : undefined} onClick={() => onChange(t.id)} onKeyDown={(e) => onKey(e, i)}>
          {t.label}
        </button>
      ))}
    </div>
  );
}

/** Loads once per key; `reload` re-fetches. Aborts the previous request on change/unmount. */
export function useLoad<T>(load: (signal: AbortSignal) => Promise<T>, key: string) {
  const [state, setState] = useState<{ key: string; data: T | null; error: unknown }>({ key, data: null, error: null });
  const [tick, setTick] = useState(0);
  useEffect(() => {
    const ctl = new AbortController();
    setState((s) => ({ key, data: s.key === key ? s.data : null, error: null }));
    load(ctl.signal).then(
      (data) => { if (!ctl.signal.aborted) setState({ key, data, error: null }); },
      (error: unknown) => { if (!ctl.signal.aborted) setState({ key, data: null, error }); },
    );
    return () => ctl.abort();
  }, [key, tick]);
  const current = state.key === key ? state : { key, data: null, error: null };
  return {
    data: current.data,
    error: current.error,
    reload: () => setTick((t) => t + 1),
    set: (data: T) => setState({ key, data, error: null }),
  };
}

export const GRANT_LABEL: Record<string, string> = {
  'admin.members': 'Kelola anggota',
  'admin.settings': 'Kelola pengaturan & katalog',
  'integration.manage': 'Kelola integrasi',
  'integration.credentials': 'Kelola kredensial integrasi',
  'integration.recovery': 'Pemulihan integrasi',
};

export const ROLE_LABEL: Record<'cs' | 'lead_cs', string> = { cs: 'CS', lead_cs: 'Lead CS' };

export const TZ_LABEL: Record<string, string> = {
  'Asia/Jakarta': 'Waktu Jakarta (WIB)',
  'Asia/Makassar': 'Waktu Makassar (WITA)',
  'Asia/Jayapura': 'Waktu Jayapura (WIT)',
};
