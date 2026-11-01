import { useEffect, useId, useState } from 'react';
import { ApiError, rateLimitText, isAbort, type Api } from '../api/client';
import type { AssignmentEvent, Member, TicketListItem, Workspace, WorkspacePermissions } from '../api/types';
import { instantLabel } from '../app/format';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';

const KIND_LABEL: Record<AssignmentEvent['kind'], string> = {
  initial: 'PIC awal saat tiket dibuat',
  take_for_self: 'Diambil sendiri',
  reassign: 'Dialihkan',
};

/**
 * Assign / reassign the CS PIC of an existing ticket. Mirrors the server rule: taking an
 * unassigned ticket for yourself needs no reason; any other change needs ticket.assign_any and
 * a reason. The ticket version guards against a concurrent change (409 keeps the input).
 */
export function AssignDialog({ api, workspace, userId, permissions, ticket, onClose, onDone, onReloadList }: {
  api: Api;
  workspace: Workspace;
  userId: string;
  permissions: WorkspacePermissions;
  ticket: TicketListItem;
  onClose: () => void;
  onDone: (t: TicketListItem, message: string) => void;
  onReloadList: () => void;
}) {
  const id = useId();
  const canAny = permissions.permissions.includes('ticket.assign_any');
  const canSelf = permissions.permissions.includes('ticket.assign_self');
  const current = ticket.owner?.id ?? '';
  const [members, setMembers] = useState<Member[] | null>(null);
  const [history, setHistory] = useState<AssignmentEvent[] | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [to, setTo] = useState<string>(!current && canSelf ? userId : current);
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<{ title: string; body: string; conflict: boolean; field?: 'reason' | 'to' } | null>(null);

  useEffect(() => {
    const ctl = new AbortController();
    api.listMembers(workspace.id, ctl.signal).then(setMembers, (e: unknown) => {
      if (!isAbort(e)) setLoadError('Daftar anggota belum dapat dimuat.');
    });
    api.getAssignmentHistory(workspace.id, ticket.id, ctl.signal).then((h) => setHistory(h.items), (e: unknown) => {
      if (!isAbort(e)) setHistory([]);
    });
    return () => ctl.abort();
  }, [api, workspace.id, ticket.id]);

  const active = (members ?? []).filter((m) => m.active);
  const selfMember = active.find((m) => m.userId === userId);
  // What the caller may pick: assign_any → any active member or nobody; assign_self → only
  // themselves, and only while the ticket is unassigned.
  const choices = canAny ? active : !current && canSelf && selfMember ? [selfMember] : [];
  const takeForSelf = !current && to === userId && canSelf;
  const needsReason = !takeForSelf;
  const unchanged = to === current;
  const allowed = canAny || (!current && canSelf && !!selfMember);

  async function submit() {
    if (busy || unchanged) return;
    if (needsReason && !reason.trim()) {
      setError({ title: 'Alasan wajib diisi', body: 'Mengalihkan atau melepas PIC memerlukan alasan.', conflict: false, field: 'reason' });
      document.getElementById(`${id}-reason`)?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await api.assignTicket(workspace.id, ticket.id, {
        ownerUserId: to || null,
        version: ticket.version,
        ...(needsReason ? { reason: reason.trim() } : {}),
      });
      const who = res.ticket.owner?.label ?? 'tanpa PIC';
      onDone(res.ticket, `${res.ticket.displayNumber} sekarang ${res.ticket.owner ? `ditangani ${who}` : 'belum ditugaskan'}. Perubahan tersimpan dan tercatat di audit.`);
    } catch (e) {
      setBusy(false);
      setError(assignError(e));
    }
  }

  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title="Ubah PIC" initialFocus={`#${CSS.escape(`${id}-to`)}`}
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button variant="primary" onClick={() => void submit()} disabled={busy || unchanged || !allowed} aria-busy={busy}>
            {busy ? 'Menyimpan…' : takeForSelf ? 'Ambil tiket' : 'Simpan PIC'}
          </Button>
        </>
      )}>
      <p className="muted">{ticket.displayNumber} · {ticket.subject}</p>
      <p>PIC saat ini: <strong>{ticket.owner?.label ?? 'Belum ditugaskan'}</strong></p>
      {error && (
        <div className="state-error spaced-bottom" role="alert">
          <h3>{error.title}</h3>
          <p>{error.body}</p>
          {error.conflict && <Button onClick={() => { onReloadList(); onClose(); }}>Muat ulang data</Button>}
        </div>
      )}
      {loadError && <p className="field-error" role="alert">{loadError}</p>}
      {!allowed ? (
        <p>Peran Anda tidak dapat mengubah PIC tiket ini{current ? ' (mengalihkan PIC memerlukan Lead CS)' : ''}.</p>
      ) : (
        <form className="formgrid" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
          <div className="field full">
            <label htmlFor={`${id}-to`}>PIC baru</label>
            <select id={`${id}-to`} className="field-control" value={to} disabled={busy || !members}
              aria-describedby={`${id}-to-hint`} onChange={(e) => { setTo(e.target.value); setError(null); }}>
              {canAny && <option value="">Belum ditugaskan (lepas PIC)</option>}
              {current && !choices.some((m) => m.userId === current) && <option value={current}>{ticket.owner?.label} (tidak aktif)</option>}
              {choices.map((m) => (
                <option key={m.userId} value={m.userId}>{m.displayName}{m.userId === userId ? ' (saya)' : ''}</option>
              ))}
            </select>
            <span id={`${id}-to-hint`} className="field-hint">Hanya anggota aktif workspace {workspace.code}.</span>
          </div>
          {needsReason && !unchanged && (
            <div className="field full">
              <label htmlFor={`${id}-reason`}>Alasan pengalihan (wajib)</label>
              <textarea id={`${id}-reason`} className="field-control" value={reason} maxLength={500} disabled={busy}
                aria-invalid={error?.field === 'reason' ? true : undefined}
                onChange={(e) => setReason(e.target.value)} />
              <span className="field-hint">Tersimpan di riwayat PIC.</span>
            </div>
          )}
          <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
        </form>
      )}
      <section className="spaced" aria-labelledby={`${id}-hist`}>
        <h3 id={`${id}-hist`}>Riwayat PIC</h3>
        {history === null ? <p className="muted tiny" role="status">Memuat riwayat…</p>
          : history.length === 0 ? <p className="muted tiny">Belum ada perubahan PIC tercatat.</p>
            : (
              <ol className="plain history">
                {history.map((h) => (
                  <li key={h.id} className="activity">
                    <strong>{KIND_LABEL[h.kind]}</strong>
                    <span className="tiny muted"> · {instantLabel(h.at, workspace.timezone)}{h.actor ? ` · oleh ${h.actor.label}` : ''}</span>
                    <p className="tiny">{h.from?.label ?? 'Belum ditugaskan'} → {h.to?.label ?? 'Belum ditugaskan'}</p>
                    {h.reason && <p className="tiny muted">Alasan: {h.reason}</p>}
                  </li>
                ))}
              </ol>
            )}
      </section>
    </Dialog>
  );
}

function assignError(e: unknown): { title: string; body: string; conflict: boolean } {
  if (e instanceof ApiError) {
    const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
    switch (e.status) {
      case 429:
        return { title: 'Terlalu banyak permintaan', body: `${rateLimitText(e)} PIC belum berubah; pilihan dan alasan Anda tetap di sini.${rid}`, conflict: false };
      case 409:
        return { title: 'Tiket sudah diubah pihak lain', body: `PIC tidak disimpan. Pilihan dan alasan Anda tetap di sini; muat ulang data lalu coba lagi.${rid}`, conflict: true };
      case 403:
        return { title: 'Tidak diizinkan', body: `Peran Anda tidak dapat melakukan perubahan PIC ini. Tidak ada yang berubah.${rid}`, conflict: false };
      case 422:
        return { title: 'Ditolak aturan proses', body: `PIC harus anggota aktif dan tiket tidak boleh berstatus arsip. Tidak ada yang berubah.${rid}`, conflict: false };
      case 400:
        return { title: 'Isian tidak valid', body: `Periksa alasan lalu coba lagi. Tidak ada yang berubah.${rid}`, conflict: false };
      case 404:
        return { title: 'Tiket tidak ditemukan', body: `Tiket ini tidak ada atau tidak dapat Anda akses.${rid}`, conflict: true };
      default:
        return { title: 'Belum tersimpan', body: `${e.message} PIC belum berubah; coba lagi.${rid}`, conflict: false };
    }
  }
  return { title: 'Belum tersimpan', body: 'Terjadi kesalahan tak terduga. PIC belum berubah.', conflict: false };
}
