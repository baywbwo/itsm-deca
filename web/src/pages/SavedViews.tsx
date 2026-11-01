import { useEffect, useRef, useState } from 'react';
import { ApiError, isAbort, type Api } from '../api/client';
import type { SavedView } from '../api/types';
import { paramsFromSavedView, sameSavedScope, savedViewQuery, type TicketParams } from '../app/routes';
import { Badge } from '../components/Badge';
import { Button } from '../components/Button';
import { Popover } from '../components/Popover';

const MAX_NAME = 60;
const MAX_VIEWS = 50;

type Load = { status: 'loading' } | { status: 'error'; error: unknown } | { status: 'ready'; items: SavedView[] };

const byName = (a: SavedView, b: SavedView) => a.name.localeCompare(b.name, 'id', { sensitivity: 'base' });
const taken = (items: SavedView[], name: string, except?: string) =>
  items.some((v) => v.id !== except && v.name.toLocaleLowerCase('id') === name.toLocaleLowerCase('id'));

function failure(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === 'version_conflict') return 'View ini sudah diubah di tab lain. Daftar dimuat ulang; coba lagi.';
    if (e.code === 'rule_violation') return 'Nama sudah dipakai atau batas 50 view tercapai.';
    if (e.code === 'invalid_request') return 'Filter atau nama tidak valid. Periksa lalu coba lagi.';
    if (e.code === 'not_found') return 'View tidak ditemukan; mungkin sudah dihapus. Daftar dimuat ulang.';
    if (e.code === 'network') return 'Server tidak dapat dihubungi. Coba lagi.';
  }
  return 'Permintaan gagal. Coba lagi.';
}

/**
 * C32 saved view: the caller's own presets for this workspace (stored by the server per
 * user+workspace, never in browser storage). A view holds the view mode, filters and sort, not
 * the search text. Applying one only changes the URL; the List/Board API still decides what is
 * visible. Rename/delete act on own views only (the server answers 404 for anything else).
 */
export function SavedViews({ api, workspaceId, params, onApply }: {
  api: Api;
  workspaceId: string;
  params: TicketParams;
  onApply: (p: TicketParams) => void;
}) {
  const [load, setLoad] = useState<Load>({ status: 'loading' });
  const [attempt, setAttempt] = useState(0);
  const [name, setName] = useState('');
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{ tone: 'ok' | 'error'; text: string } | null>(null);
  const [editing, setEditing] = useState<{ id: string; name: string } | null>(null);
  const [confirming, setConfirming] = useState<string | null>(null);
  const editRef = useRef<HTMLInputElement>(null);
  const newRef = useRef<HTMLInputElement>(null);
  const listRef = useRef<HTMLUListElement>(null);
  // Inline edit/confirm swaps the row's controls: move focus to the control that replaced them.
  const [focusNext, setFocusNext] = useState<string | null>(null);
  useEffect(() => {
    if (!focusNext) return;
    listRef.current?.querySelector<HTMLElement>(focusNext)?.focus();
    setFocusNext(null);
  }, [focusNext]);
  const row = (id: string) => `[data-view-id="${id}"]`;

  useEffect(() => {
    const ctl = new AbortController();
    setLoad({ status: 'loading' });
    api.listSavedViews(workspaceId, ctl.signal)
      .then((items) => setLoad({ status: 'ready', items: [...items].sort(byName) }))
      .catch((e: unknown) => { if (!isAbort(e)) setLoad({ status: 'error', error: e }); });
    return () => ctl.abort();
  }, [api, workspaceId, attempt]);
  useEffect(() => { if (editing) editRef.current?.focus(); }, [editing?.id]); // eslint-disable-line react-hooks/exhaustive-deps

  const items = load.status === 'ready' ? load.items : [];
  const current = items.find((v) => sameSavedScope(paramsFromSavedView(v), params)) ?? null;
  const put = (v: SavedView) => setLoad({ status: 'ready', items: [...items.filter((x) => x.id !== v.id), v].sort(byName) });
  const refreshAfter = (e: unknown) => {
    if (e instanceof ApiError && (e.code === 'version_conflict' || e.code === 'not_found')) setAttempt((n) => n + 1);
  };

  async function save() {
    const n = name.trim();
    if (!n) return setMessage({ tone: 'error', text: 'Isi nama view.' });
    if (taken(items, n)) return setMessage({ tone: 'error', text: `Nama "${n}" sudah dipakai.` });
    if (items.length >= MAX_VIEWS) return setMessage({ tone: 'error', text: 'Batas 50 view tercapai. Hapus salah satu dulu.' });
    setBusy(true);
    try {
      const v = await api.createSavedView(workspaceId, { name: n, view: params.view, query: savedViewQuery(params) });
      put(v);
      setName('');
      setMessage({ tone: 'ok', text: `View "${v.name}" disimpan.` });
    } catch (e) {
      setMessage({ tone: 'error', text: failure(e) });
    } finally {
      setBusy(false);
    }
  }

  async function rename(v: SavedView) {
    const n = editing?.name.trim() ?? '';
    if (!n) return setMessage({ tone: 'error', text: 'Isi nama baru.' });
    if (taken(items, n, v.id)) return setMessage({ tone: 'error', text: `Nama "${n}" sudah dipakai.` });
    setBusy(true);
    try {
      const next = await api.renameSavedView(workspaceId, v.id, { name: n, version: v.version });
      put(next);
      setEditing(null);
      setFocusNext(`${row(next.id)} .saved-apply`);
      setMessage({ tone: 'ok', text: `Nama diubah menjadi "${next.name}".` });
    } catch (e) {
      setMessage({ tone: 'error', text: failure(e) });
      refreshAfter(e);
    } finally {
      setBusy(false);
    }
  }

  async function remove(v: SavedView) {
    setBusy(true);
    try {
      await api.deleteSavedView(workspaceId, v.id);
      setLoad({ status: 'ready', items: items.filter((x) => x.id !== v.id) });
      setConfirming(null);
      setMessage({ tone: 'ok', text: `View "${v.name}" dihapus.` });
      newRef.current?.focus();
    } catch (e) {
      setMessage({ tone: 'error', text: failure(e) });
      refreshAfter(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Popover className="saved-views" label="Saved view" title="Saved view pribadi"
      onOpenChange={(o) => { if (!o) { setEditing(null); setConfirming(null); setMessage(null); } }}
      trigger={<>
        <span aria-hidden="true">☆</span> {current ? <span className="trigger-value">{current.name}</span> : 'Saved view'}
        {current && <span className="sr-only"> (view aktif)</span>}
      </>}>
      {(close) => (
        <>
          <p className="muted tiny">Hanya terlihat oleh Anda di workspace ini. Menyimpan tampilan, filter dan urutan;
            teks pencarian tidak disimpan. Jangan isi nama dengan data pribadi.</p>
          <form className="saved-new" onSubmit={(e) => { e.preventDefault(); void save(); }}>
            <label className="period grow">
              Nama view baru
              <input ref={newRef} className="field-control" value={name} maxLength={MAX_NAME} autoComplete="off"
                placeholder="mis. Email prioritas tinggi" onChange={(e) => setName(e.target.value)} />
            </label>
            <Button type="submit" variant="primary" disabled={busy || load.status !== 'ready'}>Simpan view</Button>
          </form>
          <p role="status" aria-live="polite" className={message?.tone === 'error' ? 'form-error tiny' : 'muted tiny'}>
            {message?.text ?? ''}
          </p>
          {load.status === 'loading' && <p className="muted tiny">Memuat saved view…</p>}
          {load.status === 'error' && (
            <div className="row">
              <span className="form-error tiny">Saved view gagal dimuat. {failure(load.error)}</span>
              <Button variant="ghost" onClick={() => setAttempt((n) => n + 1)}>Coba lagi</Button>
            </div>
          )}
          {load.status === 'ready' && items.length === 0 && (
            <p className="muted tiny">Belum ada saved view. Atur filter lalu simpan dengan nama.</p>
          )}
          {items.length > 0 && (
            <ul ref={listRef} className="saved-list" aria-label="Saved view Anda">
              {items.map((v) => (
                <li key={v.id} data-view-id={v.id} className={current?.id === v.id ? 'saved-item current' : 'saved-item'}>
                  {editing?.id === v.id ? (
                    <form className="saved-edit" onSubmit={(e) => { e.preventDefault(); void rename(v); }}>
                      <label className="period grow">
                        {`Nama baru untuk ${v.name}`}
                        <input ref={editRef} className="field-control" value={editing.name} maxLength={MAX_NAME}
                          autoComplete="off" onChange={(e) => setEditing({ id: v.id, name: e.target.value })} />
                      </label>
                      <Button type="submit" variant="primary" disabled={busy}>Simpan</Button>
                      <Button variant="ghost" onClick={() => { setEditing(null); setFocusNext(`${row(v.id)} .saved-apply`); }}>Batal</Button>
                    </form>
                  ) : confirming === v.id ? (
                    <div className="saved-edit" role="group" aria-label={`Konfirmasi hapus ${v.name}`}>
                      <span>Hapus “{v.name}”?</span>
                      <Button variant="primary" className="saved-confirm" disabled={busy} onClick={() => void remove(v)}>Hapus</Button>
                      <Button variant="ghost" onClick={() => { setConfirming(null); setFocusNext(`${row(v.id)} .saved-apply`); }}>Batal</Button>
                    </div>
                  ) : (
                    <>
                      <button type="button" className="saved-apply" aria-current={current?.id === v.id ? 'true' : undefined}
                        onClick={() => { onApply(paramsFromSavedView(v)); close(); }}>
                        <span className="saved-name">{v.name}</span>
                        <Badge>{v.view === 'board' ? 'Board' : 'List'}</Badge>
                      </button>
                      <Button variant="ghost" aria-label={`Ubah nama ${v.name}`}
                        onClick={() => { setConfirming(null); setEditing({ id: v.id, name: v.name }); }}>Ubah nama</Button>
                      <Button variant="ghost" aria-label={`Hapus ${v.name}`}
                        onClick={() => { setEditing(null); setConfirming(v.id); setFocusNext(`${row(v.id)} .saved-confirm`); }}>Hapus</Button>
                    </>
                  )}
                </li>
              ))}
            </ul>
          )}
        </>
      )}
    </Popover>
  );
}
