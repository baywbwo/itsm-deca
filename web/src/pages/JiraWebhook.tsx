import { useId, useState } from 'react';
import type { Api } from '../api/client';
import type { IntegrationConnection, IntegrationList, Workspace } from '../api/types';
import { instantLabel } from '../app/format';
import { webhookAddress } from '../app/integrations';
import { Button } from '../components/Button';
import { ConfirmDialog, WriteAlert, type WriteError } from './adminKit';

/** A secret the server just generated: held in component memory only, gone when the dialog closes. */
export interface RevealedSecret { secret: string; url: string }

/**
 * CP6a: inbound Jira webhook. Jira signs each delivery with this secret (HMAC-SHA256); the server
 * generates it and returns it once. Rotating invalidates the old value immediately.
 */
export function JiraWebhookSection({ api, workspace, list, connection: c, onChanged, onNotice, toError }: {
  api: Api; workspace: Workspace; list: IntegrationList; connection: IntegrationConnection;
  onChanged: (c: IntegrationConnection) => void; onNotice: (t: string) => void;
  toError: (e: unknown, what: string) => WriteError;
}) {
  const [revealed, setRevealed] = useState<RevealedSecret | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const [confirm, setConfirm] = useState<null | 'rotate' | 'delete'>(null);
  const w = c.webhook;
  if (!w) return null;
  const stored = w.state === 'stored';

  async function rotate() {
    setBusy(true);
    setError(null);
    try {
      const r = await api.rotateWebhookSecret(workspace.id, c.id, { version: c.version });
      onChanged(r.item);
      setRevealed({ secret: r.secret, url: webhookAddress(r, window.location.origin).url });
      onNotice('Secret webhook Jira dibuat. Salin sekarang; nilainya tidak ditampilkan lagi. Tercatat di audit (tanpa nilai).');
    } catch (e) {
      setError(toError(e, 'Secret webhook'));
    } finally {
      setBusy(false);
      setConfirm(null);
    }
  }
  async function remove() {
    setBusy(true);
    setError(null);
    try {
      const r = await api.deleteWebhookSecret(workspace.id, c.id, { version: c.version });
      onChanged(r.item);
      setRevealed(null);
      onNotice('Secret webhook Jira dihapus; kiriman dari Jira ditolak sampai secret baru dibuat. Tercatat di audit.');
    } catch (e) {
      setError(toError(e, 'Secret webhook'));
    } finally {
      setBusy(false);
      setConfirm(null);
    }
  }

  return (
    <>
      <WebhookPanel connection={c} timezone={workspace.timezone} origin={window.location.origin}
        canManage={list.canManageCredentials} keyring={list.keyringConfigured} busy={busy} error={error}
        revealed={revealed} onHide={() => setRevealed(null)}
        onRotate={() => (stored ? setConfirm('rotate') : void rotate())} onDelete={() => setConfirm('delete')} />
      {confirm === 'rotate' && (
        <ConfirmDialog title="Ganti secret webhook Jira?" confirmLabel="Ganti secret"
          impact="Secret lama langsung tidak berlaku: kiriman Jira ditolak sampai secret baru ditempel di pengaturan webhook Jira."
          onClose={() => setConfirm(null)} onConfirm={rotate} />
      )}
      {confirm === 'delete' && (
        <ConfirmDialog title="Hapus secret webhook Jira?" confirmLabel="Hapus secret"
          impact="Semua kiriman dari Jira ditolak. Pengiriman keluar ke Jira (buat issue, ubah status) tidak terpengaruh."
          onClose={() => setConfirm(null)} onConfirm={remove} />
      )}
    </>
  );
}

/** Stateless view (rendered directly in tests). */
export function WebhookPanel({ connection: c, timezone, origin, canManage, keyring, busy, error, revealed, onHide, onRotate, onDelete }: {
  connection: IntegrationConnection; timezone: string; origin: string; canManage: boolean; keyring: boolean;
  busy: boolean; error: WriteError | null; revealed: RevealedSecret | null;
  onHide: () => void; onRotate: () => void; onDelete: () => void;
}) {
  const id = useId();
  const [copied, setCopied] = useState(false);
  const w = c.webhook;
  if (!w) return null;
  const stored = w.state === 'stored';
  const address = webhookAddress(w, origin);
  const copy = (text: string) => {
    void navigator.clipboard?.writeText(text).then(() => setCopied(true), () => setCopied(false));
  };
  return (
    <section className="int-section" aria-labelledby={`${id}-h`}>
      <h3 id={`${id}-h`}>Webhook masuk dari Jira</h3>
      <p className="tiny muted">
        Jira mengirim komentar, perubahan status, dan penerima tugas issue ke timeline tiket terkait. Status yang dipetakan dapat memindahkan tiket aktif di board ITSM. Penyelesaian tetap ditinjau CS.
        Di Jira pilih peristiwa <b>Issue updated</b> dan <b>Comment created</b>.
      </p>
      <dl className="props spaced-bottom">
        <div className="props-row">
          <dt>Secret webhook</dt>
          <dd>{stored ? `Tersimpan${w.updatedAt ? ` · dibuat ${instantLabel(w.updatedAt, timezone)}` : ''}` : 'Belum dibuat (semua kiriman ditolak)'}</dd>
        </div>
        <div className="props-row">
          <dt>Alamat webhook</dt>
          <dd className="kv-text">
            <code>{address.url}</code>
            {address.guessed && <span className="tiny muted"> (dari alamat browser ini; pastikan dapat dijangkau Jira)</span>}
          </dd>
        </div>
      </dl>
      {revealed && (
        <div className="notice spaced-bottom" role="status">
          <p><b>Salin secret ini sekarang.</b> Tempel di kolom Secret pada webhook Jira. Nilainya tidak akan ditampilkan lagi; jika hilang, buat secret baru.</p>
          <label htmlFor={`${id}-s`} className="tiny">Secret webhook (sekali tampil)</label>
          <input id={`${id}-s`} className="field-control temp-password-value" readOnly value={revealed.secret} spellCheck={false} autoComplete="off"
            onFocus={(e) => e.currentTarget.select()} />
          <div className="row">
            <Button className="btn-small" onClick={() => copy(revealed.secret)}>Salin secret</Button>
            <Button className="btn-small" onClick={onHide}>Sudah disalin, sembunyikan</Button>
          </div>
          <p className="tiny muted" aria-live="polite">{copied ? 'Secret tersalin ke clipboard.' : ''}</p>
        </div>
      )}
      {error && <WriteAlert error={error} />}
      {canManage ? (
        <div className="row">
          <Button disabled={busy || !keyring} aria-busy={busy} onClick={onRotate}
            title={!keyring ? 'Kunci enkripsi server belum dipasang' : undefined}>
            {busy ? 'Memproses…' : stored ? 'Ganti secret webhook' : 'Buat secret webhook'}
          </Button>
          {stored && <Button disabled={busy} onClick={onDelete}>Hapus secret webhook</Button>}
        </div>
      ) : (
        <p className="tiny muted">Anda tidak memegang hak kelola kredensial integrasi; minta admin perusahaan.</p>
      )}
    </section>
  );
}
