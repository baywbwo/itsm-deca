import { useEffect, useId, useRef, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type { UserListItem } from '../api/types';
import { Badge } from '../components/Badge';
import { Button } from '../components/Button';
import { DataTable } from '../components/DataTable';
import { Dialog } from '../components/Dialog';
import { TextField } from '../components/Field';
import { Icon } from '../components/Icon';
import { PageHeader } from '../components/PageHeader';
import { ErrorState, ForbiddenState, LoadingState } from '../components/States';
import { initials } from '../app/labels';
import { ACTIVE_FILTERS, userWriteError, type ActiveFilter, type UserWriteError } from '../app/users';
import { ConfirmDialog, useLoad } from './adminKit';

/** A temporary password on screen: only while its dialog is open, never logged or kept after. */
interface Secret { title: string; who: string; username: string; password: string }

const CREATED = new Intl.DateTimeFormat('id-ID', { day: 'numeric', month: 'short', year: 'numeric' });

type Pending = { kind: 'reset' | 'deactivate'; user: UserListItem };

/**
 * CP3b platform user administration (US-1.3/1.5/1.7, password mode, platform_admin only). The
 * server decides access; this page only exists in the nav when the session says platformAdmin.
 */
export function UsersPage({ api, userId, onNotice }: { api: Api; userId: string; onNotice: (t: string) => void }) {
  const [text, setText] = useState('');
  const [q, setQ] = useState('');
  const [active, setActive] = useState<ActiveFilter>('all');
  const key = JSON.stringify([q, active]);
  const res = useLoad((s) => api.listUsers({ q, active }, null, s), key);
  // Later pages of the same scope; a new search or filter starts over from the first page.
  const [more, setMore] = useState<{ key: string; items: UserListItem[]; cursor: string | null } | null>(null);
  const [moreError, setMoreError] = useState<string | null>(null);
  const [loadingMore, setLoadingMore] = useState(false);
  // Rows changed here (deactivated) until the next reload.
  const [changed, setChanged] = useState<Record<string, UserListItem>>({});
  const [adding, setAdding] = useState(false);
  const [pending, setPending] = useState<Pending | null>(null);
  const [actionError, setActionError] = useState<UserWriteError | null>(null);
  const [secret, setSecret] = useState<Secret | null>(null);

  useEffect(() => { setMore(null); setMoreError(null); setChanged({}); }, [key]);

  const reload = () => { setMore(null); setChanged({}); res.reload(); };
  const first = res.data;
  const extra = more?.key === key ? more : null;
  const seen = new Set(first?.items.map((u) => u.id));
  const rows = first ? [...first.items, ...(extra?.items.filter((u) => !seen.has(u.id)) ?? [])].map((u) => changed[u.id] ?? u) : [];
  const cursor = extra ? extra.cursor : first?.nextCursor ?? null;

  async function loadMore() {
    if (!cursor || loadingMore) return;
    setLoadingMore(true);
    setMoreError(null);
    try {
      const page = await api.listUsers({ q, active }, cursor);
      setMore((m) => ({ key, items: [...(m?.key === key ? m.items : []), ...page.items], cursor: page.nextCursor }));
    } catch (e) {
      setMoreError(userWriteError(e, 'create').text.replace('Tidak ada yang berubah.', 'Coba lagi.'));
    } finally {
      setLoadingMore(false);
    }
  }

  async function confirm(p: Pending) {
    setActionError(null);
    const name = p.user.displayName;
    try {
      if (p.kind === 'reset') {
        const r = await api.resetUserPassword(p.user.id);
        setPending(null);
        setSecret({ title: 'Kata sandi sementara baru', who: name, username: p.user.username ?? '', password: r.temporaryPassword });
        setChanged((c) => ({ ...c, [p.user.id]: { ...p.user, mustChangePassword: true, locked: false } }));
        onNotice(`Kata sandi ${name} direset; semua sesinya diakhiri. Tercatat di audit.`);
      } else {
        await api.deactivateUser(p.user.id);
        setPending(null);
        setChanged((c) => ({ ...c, [p.user.id]: { ...p.user, active: false } }));
        onNotice(`${name} dinonaktifkan; semua sesinya diakhiri. Tercatat di audit.`);
      }
    } catch (e) {
      const err = userWriteError(e, p.kind);
      setActionError(err);
      if (err.reload) reload();
    }
  }

  const header = (
    <PageHeader title="Pengguna"
      description="Akun yang dapat masuk ke aplikasi · kata sandi sementara hanya ditampilkan sekali"
      action={<Button variant="primary" onClick={() => setAdding(true)}><Icon name="plus" size={16} /> Tambah pengguna</Button>} />
  );
  if (res.error) {
    const e = res.error;
    if (e instanceof ApiError && e.status === 403) return <ForbiddenState />;
    return (
      <>
        {header}
        <ErrorState title="Daftar pengguna belum dapat dimuat"
          body={e instanceof ApiError ? (e.status === 400 ? 'Pencarian atau filter tidak valid. Ubah lalu coba lagi.' : e.message) : 'Terjadi kesalahan.'}
          requestId={e instanceof ApiError ? e.requestId : null} onRetry={reload} />
      </>
    );
  }

  return (
    <>
      {header}
      <section className="pcard users-card" aria-labelledby="users-title">
        <div className="hd">
          <h2 id="users-title">Daftar pengguna</h2>
          {first && <span className="sub">{rows.length}{cursor ? '+' : ''} akun{q || active !== 'all' ? ' cocok' : ''}</span>}
          <div className="r usr-tools" role="search" aria-label="Cari dan filter pengguna">
            <form className="toolbar-search" onSubmit={(e) => { e.preventDefault(); setQ(text.trim()); }}>
              <label>
                <span className="sr-only">Cari pengguna</span>
                <input className="field-control" type="search" value={text} maxLength={100}
                  placeholder="Cari nama, nama pengguna, atau email…" onChange={(e) => {
                    setText(e.target.value);
                    if (!e.target.value.trim()) setQ('');
                  }} />
              </label>
            </form>
            <label className="toolbar-select">
              <span className="sr-only">Status akun</span>
              <select className="field-control" value={active} onChange={(e) => setActive(e.target.value as ActiveFilter)}>
                {ACTIVE_FILTERS.map((f) => <option key={f.value} value={f.value}>{f.label}</option>)}
              </select>
            </label>
          </div>
        </div>
        {!first ? <div className="bd"><LoadingState label="Memuat pengguna…" /></div> : (
          <>
            <div className="ptable">
              <DataTable caption="Daftar pengguna platform" rows={rows} rowKey={(u) => u.id}
                empty={q || active !== 'all'
                  ? { title: 'Tidak ada pengguna yang cocok', body: 'Ubah kata kunci atau filter status.' }
                  : { title: 'Belum ada pengguna', body: 'Tambah pengguna untuk memberi akses masuk.' }}
                columns={[
                  {
                    key: 'name', header: 'Nama',
                    cell: (u) => (
                      <>
                        <span className="cell-person">
                          <span className="avatar" aria-hidden="true">{initials(u.displayName)}</span>
                          <span><span><b>{u.displayName}</b>{u.id === userId ? ' (saya)' : ''}</span>
                            <span className="tiny muted usr-login">{u.username ?? 'Tanpa kata sandi lokal'}</span></span>
                        </span>
                      </>
                    ),
                  },
                  { key: 'email', header: 'Email', cell: (u) => u.email ?? <span className="muted">—</span> },
                  {
                    key: 'status', header: 'Status',
                    cell: (u) => (
                      <div className="row">
                        <Badge tone={u.active ? 'ok' : 'neutral'}>{u.active ? 'Aktif' : 'Nonaktif'}</Badge>
                        {u.platformAdmin && <Badge tone="blue">Admin platform</Badge>}
                        {u.active && u.mustChangePassword && <Badge tone="waiting">Wajib ganti sandi</Badge>}
                        {u.active && u.locked && <Badge tone="error">Terkunci</Badge>}
                    </div>
                  ),
                },
                { key: 'created', header: 'Dibuat', cell: (u) => CREATED.format(new Date(u.createdAt)) },
                {
                  key: 'act', header: 'Aksi',
                  cell: (u) => {
                    if (u.id === userId) return <span className="tiny muted">Pakai menu akun</span>;
                    if (!u.active) return <span className="tiny muted">—</span>;
                    return (
                      <div className="row usr-actions">
                        {u.username && (
                          <Button className="btn-small" aria-label={`Reset kata sandi ${u.displayName}`}
                            onClick={() => { setActionError(null); setPending({ kind: 'reset', user: u }); }}>Reset kata sandi</Button>
                        )}
                        <Button className="btn-small" aria-label={`Nonaktifkan ${u.displayName}`}
                          onClick={() => { setActionError(null); setPending({ kind: 'deactivate', user: u }); }}>Nonaktifkan</Button>
                      </div>
                    );
                  },
                },
              ]} />
            </div>
            {(cursor || extra) && (
              <div className="row bd usr-more">
                <Button onClick={() => void loadMore()} disabled={!cursor || loadingMore} aria-busy={loadingMore}>
                  {loadingMore ? 'Memuat…' : cursor ? 'Muat lebih banyak' : 'Semua sudah dimuat'}
                </Button>
                {moreError && <span role="alert" className="tiny">{moreError}</span>}
              </div>
            )}
          </>
        )}
      </section>
      {adding && (
        <CreateUserDialog api={api} onClose={() => setAdding(false)} onCreated={(created) => {
          setAdding(false);
          setSecret({ title: 'Pengguna dibuat', who: created.displayName, username: created.username, password: created.password });
          onNotice(`${created.displayName} ditambahkan dan wajib mengganti kata sandi saat masuk pertama. Tercatat di audit.`);
          reload();
        }} />
      )}
      {pending && (
        <ConfirmDialog
          title={pending.kind === 'reset' ? `Reset kata sandi ${pending.user.displayName}?` : `Nonaktifkan ${pending.user.displayName}?`}
          impact={pending.kind === 'reset'
            ? 'Kata sandi lama tidak berlaku lagi dan semua sesi pengguna ini diakhiri. Kata sandi sementara baru ditampilkan sekali; pengguna wajib menggantinya saat masuk.'
            : 'Pengguna ini tidak dapat masuk lagi dan semua sesinya diakhiri. Riwayat tiket dan audit tetap tersimpan. Pengaktifan kembali belum tersedia di aplikasi.'}
          confirmLabel={pending.kind === 'reset' ? 'Reset kata sandi' : 'Nonaktifkan'}
          onConfirm={() => confirm(pending)} onClose={() => { setPending(null); setActionError(null); }}>
          <p className="tiny muted">{pending.user.username ?? 'Tanpa kata sandi lokal'}{pending.user.email ? ` · ${pending.user.email}` : ''}</p>
          {actionError && <p role="alert" className="auth-error">{actionError.text}</p>}
        </ConfirmDialog>
      )}
      {secret && <TemporaryPasswordDialog secret={secret} onClose={() => setSecret(null)} />}
    </>
  );
}

function CreateUserDialog({ api, onClose, onCreated }: {
  api: Api;
  onClose: () => void;
  onCreated: (u: { displayName: string; username: string; password: string }) => void;
}) {
  const [username, setUsername] = useState('');
  const [displayName, setDisplayName] = useState('');
  const [email, setEmail] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<UserWriteError | null>(null);
  const formId = useId();
  const formRef = useRef<HTMLFormElement>(null);

  useEffect(() => {
    if (!busy && error?.field) formRef.current?.querySelector<HTMLInputElement>(`input[name="${error.field}"]`)?.focus();
  }, [busy, error]);

  async function submit() {
    if (busy || !username.trim() || !displayName.trim()) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.createUser({ username: username.trim(), displayName: displayName.trim(), email: email.trim() || null });
      onCreated({ displayName: r.user.displayName, username: r.user.username, password: r.temporaryPassword });
    } catch (e) {
      setError(userWriteError(e, 'create'));
      setBusy(false);
    }
  }

  const fieldError = (f: string) => (error?.field === f ? error.text : undefined);
  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title="Tambah pengguna" initialFocus="input"
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button type="submit" form={formId} variant="primary" disabled={busy || !username.trim() || !displayName.trim()} aria-busy={busy}>
            {busy ? 'Menyimpan…' : 'Tambah pengguna'}
          </Button>
        </>
      )}>
      {error && !error.field && <p role="alert" className="auth-error">{error.text}</p>}
      <form ref={formRef} id={formId} className="formgrid" noValidate onSubmit={(e) => { e.preventDefault(); void submit(); }}>
        <TextField className="full" label="Nama pengguna" name="username" required autoComplete="off" maxLength={254}
          value={username} disabled={busy} onChange={(e) => setUsername(e.target.value)}
          hint="Dipakai untuk masuk. Tanpa spasi; huruf besar dan kecil dianggap sama." error={fieldError('username')} />
        <TextField className="full" label="Nama tampilan" name="displayName" required maxLength={200}
          value={displayName} disabled={busy} onChange={(e) => setDisplayName(e.target.value)} error={fieldError('displayName')} />
        <TextField className="full" label="Email (opsional)" name="email" type="email" autoComplete="off" maxLength={254}
          value={email} disabled={busy} onChange={(e) => setEmail(e.target.value)} error={fieldError('email')} />
      </form>
      <p className="tiny muted">Sistem membuat kata sandi sementara yang ditampilkan sekali setelah disimpan. Pengguna wajib menggantinya saat masuk pertama.</p>
    </Dialog>
  );
}

/** One-time display: copy button, warning, and the value dropped from state when the dialog closes. */
export function TemporaryPasswordDialog({ secret, onClose }: { secret: Secret; onClose: () => void }) {
  const [copied, setCopied] = useState<'ok' | 'failed' | null>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  async function copy() {
    try {
      await navigator.clipboard.writeText(secret.password);
      setCopied('ok');
    } catch {
      inputRef.current?.select();
      setCopied('failed');
    }
  }
  return (
    <Dialog open onClose={onClose} title={secret.title} initialFocus=".temp-copy"
      footer={<Button variant="primary" onClick={onClose}>Saya sudah menyimpannya</Button>}>
      <p>Kata sandi sementara untuk <strong>{secret.who}</strong>{secret.username ? <> (<code>{secret.username}</code>)</> : null}:</p>
      <div className="row temp-password">
        <label className="sr-only" htmlFor="temp-password">Kata sandi sementara</label>
        <input ref={inputRef} id="temp-password" className="field-control temp-password-value" readOnly value={secret.password}
          autoComplete="off" spellCheck={false} onFocus={(e) => e.currentTarget.select()} />
        <Button className="temp-copy" onClick={() => void copy()}>Salin</Button>
      </div>
      <p role="status" className="tiny">
        {copied === 'ok' ? 'Tersalin ke papan klip.' : copied === 'failed' ? 'Tidak dapat menyalin otomatis; teks sudah dipilih, salin manual.' : ''}
      </p>
      <div className="state-warn" role="note">
        <h3>Tidak dapat dilihat lagi</h3>
        <p>Kata sandi ini hanya ditampilkan sekali dan tidak disimpan dalam bentuk terbaca. Serahkan lewat kanal aman;
          bila hilang, lakukan reset kata sandi lagi.</p>
      </div>
    </Dialog>
  );
}
