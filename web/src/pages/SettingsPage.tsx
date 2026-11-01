import { useId, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type {
  CatalogEditor, CatalogEntry, CatalogKind, GrantCode, Member, MemberList, SettingsBody, Timezone,
  Workspace, WorkspaceSettings,
} from '../api/types';
import { Badge } from '../components/Badge';
import { Button } from '../components/Button';
import { DataTable } from '../components/DataTable';
import { Dialog } from '../components/Dialog';
import { SelectField, TextField } from '../components/Field';
import { PageHeader } from '../components/PageHeader';
import { ErrorState, ForbiddenState, LoadingState } from '../components/States';
import {
  ConfirmDialog, GRANT_LABEL, ROLE_LABEL, Tabs, TZ_LABEL, useLoad, WriteAlert, writeError, type WriteError,
} from './adminKit';

type TabId = 'workspace' | 'members' | 'catalogs' | 'companies';

/**
 * O07/O09 Pengaturan. Every tab reads its own server answer; edit controls appear only when the
 * server says so (`canEdit`/`canManage`), and the server re-checks each write (default deny).
 */
export function SettingsPage({ api, workspace, uat, permissions, onNotice }: {
  api: Api;
  workspace: Workspace;
  uat: boolean;
  permissions: readonly string[];
  onNotice: (text: string) => void;
}) {
  const tabs: { id: TabId; label: string }[] = [
    ...(permissions.includes('catalog.read') ? [{ id: 'workspace' as const, label: 'Workspace' }] : []),
    ...(permissions.includes('member.read') ? [{ id: 'members' as const, label: 'Anggota' }] : []),
    ...(permissions.includes('catalog.read') ? [{ id: 'catalogs' as const, label: 'Kategori & referensi' }] : []),
    ...(uat ? [{ id: 'companies' as const, label: 'Perusahaan' }] : []),
  ];
  const [tab, setTab] = useState<TabId>(tabs[0]?.id ?? 'workspace');
  const panelId = useId();
  if (tabs.length === 0) return <ForbiddenState />;
  return (
    <>
      <PageHeader eyebrow={workspace.code} title="Pengaturan"
        description={`Profil workspace, anggota dan katalog referensi ${workspace.code}. Penonaktifan menggantikan penghapusan; histori tetap tersimpan.`} />
      <Tabs tabs={tabs} value={tab} onChange={setTab} label="Bagian pengaturan" panelId={panelId} />
      <div id={panelId} role="tabpanel" aria-labelledby={`${panelId}-tab-${tab}`}>
        {tab === 'workspace' && <WorkspaceTab api={api} workspace={workspace} onNotice={onNotice} />}
        {tab === 'members' && <MembersTab api={api} workspace={workspace} onNotice={onNotice} />}
        {tab === 'catalogs' && <CatalogTab api={api} workspace={workspace} onNotice={onNotice} />}
        {tab === 'companies' && <CompaniesTab api={api} workspace={workspace} onNotice={onNotice} />}
      </div>
    </>
  );
}

function LoadFailure({ error, what, onRetry }: { error: unknown; what: string; onRetry: () => void }) {
  if (error instanceof ApiError && error.status === 403) return <ForbiddenState />;
  return (
    <ErrorState title={`${what} belum dapat dimuat`} body={error instanceof ApiError ? error.message : 'Terjadi kesalahan.'}
      requestId={error instanceof ApiError ? error.requestId : null} onRetry={onRetry} />
  );
}

function ReadOnlyBanner({ what }: { what: string }) {
  return (
    <p className="banner spaced-bottom" role="note">
      Mode baca saja. {what} hanya dapat diubah oleh anggota yang diberi hak eksplisit oleh pengelola workspace;
      peran Lead CS tidak otomatis menjadi admin.
    </p>
  );
}

// ---------------------------------------------------------------- Workspace profile

function WorkspaceTab({ api, workspace, onNotice }: { api: Api; workspace: Workspace; onNotice: (t: string) => void }) {
  const res = useLoad((s) => api.getSettings(workspace.id, s), workspace.id);
  if (res.error) return <LoadFailure error={res.error} what="Profil workspace" onRetry={res.reload} />;
  if (!res.data) return <LoadingState label="Memuat profil workspace…" />;
  return <WorkspaceForm key={res.data.settings.version} api={api} body={res.data} onReload={res.reload}
    onSaved={(b) => { res.set(b); onNotice('Profil workspace tersimpan dan tercatat di audit.'); }} />;
}

function WorkspaceForm({ api, body, onReload, onSaved }: {
  api: Api;
  body: SettingsBody;
  onReload: () => void;
  onSaved: (b: SettingsBody) => void;
}) {
  const s = body.settings;
  const [form, setForm] = useState({
    workspaceName: s.workspaceName, companyName: s.companyName, timezone: s.timezone as Timezone,
    reporterCode: s.reporterCode ?? '',
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const dirty = form.workspaceName !== s.workspaceName || form.companyName !== s.companyName ||
    form.timezone !== s.timezone || form.reporterCode !== (s.reporterCode ?? '');

  async function save() {
    if (busy || !dirty) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.updateSettings(s.workspaceId, {
        workspaceName: form.workspaceName.trim(), companyName: form.companyName.trim(), timezone: form.timezone,
        reporterCode: form.reporterCode.trim() || null, version: s.version,
      });
      onSaved({ ...body, settings: r.settings });
    } catch (e) {
      setBusy(false);
      setError(writeError(e, 'Profil workspace'));
    }
  }

  const props: [string, string][] = [
    ['Kode workspace', s.workspaceCode],
    ['Kode perusahaan', s.companyCode],
    ['Status', s.active ? 'Aktif' : 'Nonaktif'],
    ['Anggota aktif', String(s.memberCount)],
  ];
  return (
    <section className="card" aria-labelledby="ws-profile">
      <h2 id="ws-profile">Profil workspace</h2>
      {!body.canEdit && <ReadOnlyBanner what="Profil workspace" />}
      {error && <WriteAlert error={error} onReload={onReload} />}
      <dl className="props spaced-bottom">
        {props.map(([k, v]) => <div key={k} className="props-row"><dt>{k}</dt><dd>{v}</dd></div>)}
      </dl>
      {body.canEdit ? (
        <form className="formgrid" onSubmit={(e) => { e.preventDefault(); void save(); }}>
          <TextField label="Nama workspace" required maxLength={200} value={form.workspaceName} disabled={busy}
            onChange={(e) => setForm({ ...form, workspaceName: e.target.value })} />
          <TextField label="Nama perusahaan" required maxLength={200} value={form.companyName} disabled={busy}
            onChange={(e) => setForm({ ...form, companyName: e.target.value })} />
          <SelectField label="Zona waktu" value={form.timezone} disabled={busy}
            onChange={(e) => setForm({ ...form, timezone: e.target.value as Timezone })}>
            {body.timezones.map((t) => <option key={t} value={t}>{TZ_LABEL[t] ?? t}</option>)}
          </SelectField>
          <TextField label="Kode pelapor" maxLength={40} value={form.reporterCode} disabled={busy}
            hint="Opsional. Isi hanya kode yang sudah diverifikasi kantor; tidak ada format yang ditebak."
            error={error?.field === 'reporterCode' ? 'Periksa kode pelapor.' : undefined}
            onChange={(e) => setForm({ ...form, reporterCode: e.target.value })} />
          <div className="full row">
            <Button type="submit" variant="primary" disabled={busy || !dirty} aria-busy={busy}>
              {busy ? 'Menyimpan…' : 'Simpan profil'}
            </Button>
            {dirty && !busy && <span className="tiny muted">Ada perubahan yang belum disimpan.</span>}
          </div>
        </form>
      ) : (
        <dl className="props">
          <div className="props-row"><dt>Nama workspace</dt><dd>{s.workspaceName}</dd></div>
          <div className="props-row"><dt>Nama perusahaan</dt><dd>{s.companyName}</dd></div>
          <div className="props-row"><dt>Zona waktu</dt><dd>{TZ_LABEL[s.timezone] ?? s.timezone}</dd></div>
          <div className="props-row"><dt>Kode pelapor</dt><dd>{s.reporterCode ?? 'Belum diisi'}</dd></div>
        </dl>
      )}
    </section>
  );
}

// ---------------------------------------------------------------- Members

function MembersTab({ api, workspace, onNotice }: { api: Api; workspace: Workspace; onNotice: (t: string) => void }) {
  const res = useLoad((s) => api.getMemberList(workspace.id, s), workspace.id);
  const [editing, setEditing] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  if (res.error) return <LoadFailure error={res.error} what="Daftar anggota" onRetry={res.reload} />;
  const list = res.data;
  if (!list) return <LoadingState label="Memuat anggota…" />;
  const replace = (m: Member) => res.set({ ...list, items: list.items.map((x) => (x.membershipId === m.membershipId ? m : x)) });
  const target = list.items.find((m) => m.membershipId === editing) ?? null;
  return (
    <section aria-labelledby="members-title">
      <div className="row between spaced-bottom">
        <h2 id="members-title">Anggota {workspace.code}</h2>
        {list.canManage && <Button variant="primary" onClick={() => setAdding(true)}>＋ Tambah anggota</Button>}
      </div>
      {!list.canManage && <ReadOnlyBanner what="Keanggotaan" />}
      <DataTable caption={`Anggota workspace ${workspace.code}`} rows={list.items} rowKey={(m) => m.membershipId}
        empty={{ title: 'Belum ada anggota', body: 'Workspace ini belum memiliki anggota.' }}
        columns={[
          { key: 'name', header: 'Nama', cell: (m) => <>{m.displayName}{m.membershipId === list.myMembershipId ? ' (saya)' : ''}</> },
          { key: 'role', header: 'Peran', cell: (m) => ROLE_LABEL[m.role] },
          { key: 'status', header: 'Status', cell: (m) => <Badge tone={m.active ? 'ok' : 'neutral'}>{m.active ? 'Aktif' : 'Nonaktif'}</Badge> },
          ...(list.canManage ? [{
            key: 'grants', header: 'Hak tambahan',
            cell: (m: Member) => (m.grants.length ? m.grants.map((g) => GRANT_LABEL[g] ?? g).join(', ') : 'Tidak ada'),
          }] : []),
          {
            key: 'act', header: 'Aksi',
            cell: (m) => (list.canManage && m.membershipId !== list.myMembershipId
              ? <Button onClick={() => setEditing(m.membershipId)} aria-label={`Kelola ${m.displayName}`}>Kelola</Button>
              : <span className="tiny muted">{m.membershipId === list.myMembershipId ? 'Tidak dapat mengubah diri sendiri' : '—'}</span>),
          },
        ]} />
      {adding && (
        <AddMemberDialog api={api} workspace={workspace} onClose={() => setAdding(false)}
          onDone={(m) => { res.set({ ...list, items: [...list.items, m] }); setAdding(false); onNotice(`${m.displayName} ditambahkan sebagai ${ROLE_LABEL[m.role]}. Tercatat di audit.`); }} />
      )}
      {target && (
        <MemberDialog api={api} workspace={workspace} list={list} member={target} onClose={() => setEditing(null)}
          onChanged={replace} onReload={() => { setEditing(null); res.reload(); }} onNotice={onNotice} />
      )}
    </section>
  );
}

function AddMemberDialog({ api, workspace, onClose, onDone }: {
  api: Api; workspace: Workspace; onClose: () => void; onDone: (m: Member) => void;
}) {
  const [subject, setSubject] = useState('');
  const [role, setRole] = useState<'cs' | 'lead_cs'>('cs');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const formId = useId();
  async function submit() {
    if (busy || !subject.trim()) return;
    setBusy(true);
    setError(null);
    try {
      onDone((await api.addMember(workspace.id, { subject: subject.trim(), role })).item);
    } catch (e) {
      setBusy(false);
      setError(writeError(e, 'Anggota'));
    }
  }
  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title="Tambah anggota" initialFocus="input"
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button type="submit" form={formId} variant="primary" disabled={busy || !subject.trim()} aria-busy={busy}>
            {busy ? 'Menyimpan…' : 'Tambah'}
          </Button>
        </>
      )}>
      {error && <WriteAlert error={error} />}
      <form id={formId} className="formgrid" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
        <TextField className="full" label="Subjek akun" required maxLength={200} value={subject} disabled={busy}
          hint="Subjek akun persis (mis. dummy:cs-bravo). Tidak ada pencarian akun; subjek yang tidak dikenal ditolak."
          error={error?.field === 'subject' ? 'Subjek tidak dikenal atau sudah menjadi anggota.' : undefined}
          onChange={(e) => setSubject(e.target.value)} />
        <SelectField label="Peran" value={role} disabled={busy} onChange={(e) => setRole(e.target.value as 'cs' | 'lead_cs')}
          hint="Hak admin tidak ikut peran; diberikan terpisah setelah anggota dibuat.">
          <option value="cs">CS</option>
          <option value="lead_cs">Lead CS</option>
        </SelectField>
      </form>
    </Dialog>
  );
}

function MemberDialog({ api, workspace, list, member, onClose, onChanged, onReload, onNotice }: {
  api: Api; workspace: Workspace; list: MemberList; member: Member;
  onClose: () => void; onChanged: (m: Member) => void; onReload: () => void; onNotice: (t: string) => void;
}) {
  const [role, setRole] = useState(member.role);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const [confirm, setConfirm] = useState<null | { kind: 'deactivate' } | { kind: 'revoke'; code: GrantCode }>(null);

  async function run(what: string, f: () => Promise<{ item: Member }>, notice: string) {
    setBusy(true);
    setError(null);
    try {
      const r = await f();
      onChanged(r.item);
      onNotice(notice);
    } catch (e) {
      setError(writeError(e, what));
    } finally {
      setBusy(false);
      setConfirm(null);
    }
  }
  const patch = (active: boolean, nextRole = role) => run('Keanggotaan', () => api.updateMember(workspace.id, member.membershipId, {
    role: nextRole, active, version: member.version,
  }), `${member.displayName}: ${active ? `peran ${ROLE_LABEL[nextRole]}, aktif` : 'dinonaktifkan; hak tambahan dicabut'}. Tercatat di audit.`);

  return (
    <>
      <Dialog open onClose={busy ? () => undefined : onClose} title={`Kelola ${member.displayName}`} variant="peek" initialFocus="select"
        footer={<Button onClick={onClose} disabled={busy}>Tutup</Button>}>
        {error && <WriteAlert error={error} onReload={onReload} />}
        <p className="muted">Status: <strong>{member.active ? 'Aktif' : 'Nonaktif'}</strong> · versi {member.version}</p>
        <section className="spaced-bottom" aria-labelledby="mem-role">
          <h3 id="mem-role">Peran</h3>
          <div className="row">
            <SelectField label="Peran di workspace" value={role} disabled={busy || !member.active}
              onChange={(e) => setRole(e.target.value as 'cs' | 'lead_cs')}>
              <option value="cs">CS</option>
              <option value="lead_cs">Lead CS</option>
            </SelectField>
            <Button variant="primary" disabled={busy || role === member.role || !member.active}
              onClick={() => void patch(true)}>Simpan peran</Button>
          </div>
          <p className="tiny muted">Lead CS dapat mengalihkan PIC, tetapi bukan admin.</p>
        </section>
        <section className="spaced-bottom" aria-labelledby="mem-grants">
          <h3 id="mem-grants">Hak tambahan (eksplisit)</h3>
          {!member.active ? <p className="tiny muted">Anggota nonaktif tidak dapat diberi hak.</p> : list.grantable.length === 0
            ? <p className="tiny muted">Anda tidak memegang hak yang dapat diberikan.</p>
            : (
              <ul className="plain">
                {list.grantable.map((g) => {
                  const has = member.grants.includes(g);
                  return (
                    <li key={g} className="row between">
                      <span>{GRANT_LABEL[g] ?? g} <Badge tone={has ? 'ok' : 'neutral'}>{has ? 'Diberikan' : 'Tidak'}</Badge></span>
                      {has
                        ? <Button disabled={busy} onClick={() => setConfirm({ kind: 'revoke', code: g })}>Cabut</Button>
                        : <Button disabled={busy} onClick={() => void run('Hak', () => api.addMemberGrant(workspace.id, member.membershipId, { grantCode: g, version: member.version }),
                          `${GRANT_LABEL[g] ?? g} diberikan ke ${member.displayName}. Tercatat di audit.`)}>Beri</Button>}
                    </li>
                  );
                })}
              </ul>
            )}
          <p className="tiny muted">Anda hanya dapat memberi hak yang Anda pegang sendiri.</p>
        </section>
        <section aria-labelledby="mem-status">
          <h3 id="mem-status">Status keanggotaan</h3>
          {member.active
            ? <Button disabled={busy} onClick={() => setConfirm({ kind: 'deactivate' })}>Nonaktifkan anggota</Button>
            : <Button disabled={busy} onClick={() => void patch(true)}>Aktifkan kembali</Button>}
        </section>
      </Dialog>
      {confirm?.kind === 'deactivate' && (
        <ConfirmDialog title={`Nonaktifkan ${member.displayName}?`} confirmLabel="Nonaktifkan"
          impact={`${member.displayName} kehilangan akses ke ${workspace.code} dan semua hak tambahannya dicabut. Tiket, catatan dan riwayatnya tetap tersimpan; anggota dapat diaktifkan kembali.`}
          onClose={() => setConfirm(null)} onConfirm={() => patch(false, member.role)} />
      )}
      {confirm?.kind === 'revoke' && (
        <ConfirmDialog title="Cabut hak?" confirmLabel="Cabut hak"
          impact={`${member.displayName} tidak lagi dapat: ${GRANT_LABEL[confirm.code] ?? confirm.code}. Berlaku segera untuk permintaan berikutnya.`}
          onClose={() => setConfirm(null)}
          onConfirm={() => run('Hak', () => api.revokeMemberGrant(workspace.id, member.membershipId, { grantCode: confirm.code, version: member.version }),
            `${GRANT_LABEL[confirm.code] ?? confirm.code} dicabut dari ${member.displayName}. Tercatat di audit.`)} />
      )}
    </>
  );
}

// ---------------------------------------------------------------- Catalogs

const KINDS: readonly { id: CatalogKind; label: string }[] = [
  { id: 'categories', label: 'Kategori' },
  { id: 'products', label: 'Produk / instrumen' },
  { id: 'receiving-units', label: 'Unit penerima' },
  { id: 'regions', label: 'Wilayah' },
  { id: 'intake-channels', label: 'Kanal penerimaan' },
  { id: 'priorities', label: 'Prioritas' },
];

function CatalogTab({ api, workspace, onNotice }: { api: Api; workspace: Workspace; onNotice: (t: string) => void }) {
  const [kind, setKind] = useState<CatalogKind>('categories');
  const res = useLoad((s) => api.listCatalogEntries(workspace.id, kind, s), `${workspace.id}:${kind}`);
  const [editing, setEditing] = useState<CatalogEntry | 'new' | null>(null);
  const label = KINDS.find((k) => k.id === kind)!.label;
  const ed = res.data;
  return (
    <section aria-labelledby="cat-title">
      <div className="row between spaced-bottom">
        <h2 id="cat-title">Katalog referensi</h2>
        {ed?.canEdit && kind !== 'priorities' && <Button variant="primary" onClick={() => setEditing('new')}>＋ Tambah {label.toLowerCase()}</Button>}
      </div>
      <div className="toolbar row">
        <SelectField label="Jenis katalog" value={kind} onChange={(e) => { setKind(e.target.value as CatalogKind); setEditing(null); }}>
          {KINDS.map((k) => <option key={k.id} value={k.id}>{k.label}</option>)}
        </SelectField>
      </div>
      {res.error ? <LoadFailure error={res.error} what={`Katalog ${label}`} onRetry={res.reload} />
        : !ed ? <LoadingState label={`Memuat ${label.toLowerCase()}…`} />
          : (
            <>
              {!ed.canEdit && <ReadOnlyBanner what="Katalog" />}
              {kind === 'priorities' && <p className="tiny muted spaced-bottom">Empat prioritas baku dibuat bersama workspace; label dan urutan dapat diubah, kode tidak ditambah.</p>}
              <DataTable caption={`${label} di ${workspace.code}`} rows={ed.items} rowKey={(c) => c.id}
                empty={{ title: `Belum ada ${label.toLowerCase()}`, body: 'Tambahkan entri pertama bila Anda memiliki hak pengaturan.' }}
                columns={[
                  { key: 'code', header: 'Kode', cell: (c) => <code>{c.code}</code> },
                  { key: 'label', header: 'Label', cell: (c) => c.label },
                  { key: 'order', header: 'Urutan', cell: (c) => c.sortOrder },
                  { key: 'status', header: 'Status', cell: (c) => <Badge tone={c.active ? 'ok' : 'neutral'}>{c.active ? 'Aktif' : 'Nonaktif'}</Badge> },
                  ...(ed.canEdit ? [{
                    key: 'act', header: 'Aksi',
                    cell: (c: CatalogEntry) => <Button onClick={() => setEditing(c)} aria-label={`Ubah ${c.label}`}>Ubah</Button>,
                  }] : []),
                ]} />
            </>
          )}
      {editing && ed && (
        <CatalogDialog api={api} workspace={workspace} editor={ed} kindLabel={label} entry={editing === 'new' ? null : editing}
          onClose={() => setEditing(null)} onReload={() => { setEditing(null); res.reload(); }}
          onSaved={(item, created) => {
            res.set({ ...ed, items: created ? [...ed.items, item] : ed.items.map((x) => (x.id === item.id ? item : x)) });
            setEditing(null);
            onNotice(`${label} "${item.label}" ${created ? 'ditambahkan' : 'diperbarui'}. Tercatat di audit.`);
          }} />
      )}
    </section>
  );
}

function CatalogDialog({ api, workspace, editor, kindLabel, entry, onClose, onReload, onSaved }: {
  api: Api; workspace: Workspace; editor: CatalogEditor; kindLabel: string; entry: CatalogEntry | null;
  onClose: () => void; onReload: () => void; onSaved: (c: CatalogEntry, created: boolean) => void;
}) {
  const [code, setCode] = useState('');
  const [label, setLabel] = useState(entry?.label ?? '');
  const [order, setOrder] = useState(String(entry?.sortOrder ?? (editor.items.length + 1) * 10));
  const [active, setActive] = useState(entry?.active ?? true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const [confirmOff, setConfirmOff] = useState(false);
  const formId = useId();
  const sortOrder = Number(order);
  const valid = label.trim() !== '' && Number.isInteger(sortOrder) && sortOrder >= 0 && sortOrder <= 9999 &&
    (entry !== null || /^[a-z0-9_]{1,40}$/.test(code));

  async function save() {
    setBusy(true);
    setError(null);
    try {
      const r = entry
        ? await api.updateCatalogEntry(workspace.id, editor.kind, entry.id, { label: label.trim(), active, sortOrder, version: entry.version })
        : await api.createCatalogEntry(workspace.id, editor.kind, { code, label: label.trim(), sortOrder });
      onSaved(r.item, entry === null);
    } catch (e) {
      setBusy(false);
      setConfirmOff(false);
      setError(writeError(e, kindLabel));
    }
  }
  const submit = () => {
    if (busy || !valid) return;
    if (entry?.active && !active) setConfirmOff(true);
    else void save();
  };
  return (
    <>
      <Dialog open onClose={busy ? () => undefined : onClose} title={entry ? `Ubah ${kindLabel.toLowerCase()}` : `Tambah ${kindLabel.toLowerCase()}`}
        initialFocus="input:not([disabled])"
        footer={(
          <>
            <Button onClick={onClose} disabled={busy}>Batal</Button>
            <Button type="submit" form={formId} variant="primary" disabled={busy || !valid} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan'}</Button>
          </>
        )}>
        {error && <WriteAlert error={error} onReload={onReload} />}
        <form id={formId} className="formgrid" onSubmit={(e) => { e.preventDefault(); submit(); }}>
          {entry ? (
            <TextField label="Kode" value={entry.code} disabled hint="Kode tidak berubah setelah dibuat." />
          ) : (
            <TextField label="Kode" required maxLength={40} value={code} disabled={busy}
              hint="Huruf kecil, angka atau garis bawah; unik per workspace."
              error={error?.field === 'code' ? 'Kode sudah dipakai atau tidak valid.' : undefined}
              onChange={(e) => setCode(e.target.value.toLowerCase())} />
          )}
          <TextField label="Label" required maxLength={120} value={label} disabled={busy}
            error={error?.field === 'label' ? 'Periksa label.' : undefined} onChange={(e) => setLabel(e.target.value)} />
          <TextField label="Urutan" type="number" min={0} max={9999} value={order} disabled={busy}
            onChange={(e) => setOrder(e.target.value)} />
          {entry && (
            <SelectField label="Status" value={active ? 'true' : 'false'} disabled={busy} onChange={(e) => setActive(e.target.value === 'true')}
              hint="Entri nonaktif tidak bisa dipilih di tiket baru; tiket lama tetap menampilkannya.">
              <option value="true">Aktif</option>
              <option value="false">Nonaktif</option>
            </SelectField>
          )}
        </form>
        <p className="tiny muted spaced">Tidak ada hapus permanen; entri yang tidak dipakai lagi dinonaktifkan.</p>
      </Dialog>
      {confirmOff && entry && (
        <ConfirmDialog title={`Nonaktifkan "${entry.label}"?`} confirmLabel="Nonaktifkan"
          impact={`Entri ini tidak lagi dapat dipilih saat membuat atau mengubah tiket di ${workspace.code}. Tiket dan laporan yang sudah memakainya tidak berubah.`}
          onClose={() => setConfirmOff(false)} onConfirm={save} />
      )}
    </>
  );
}

// ---------------------------------------------------------------- Companies (UAT Superadmin)

function CompaniesTab({ api, workspace, onNotice }: { api: Api; workspace: Workspace; onNotice: (t: string) => void }) {
  const res = useLoad((s) => api.listCompanies(s), 'companies');
  const [adding, setAdding] = useState(false);
  const [toggle, setToggle] = useState<WorkspaceSettings | null>(null);
  const [error, setError] = useState<WriteError | null>(null);
  if (res.error) return <LoadFailure error={res.error} what="Daftar perusahaan" onRetry={res.reload} />;
  const list = res.data;
  if (!list) return <LoadingState label="Memuat perusahaan…" />;
  const replace = (c: WorkspaceSettings) => res.set({ items: list.items.map((x) => (x.companyId === c.companyId ? c : x)) });
  return (
    <section aria-labelledby="co-title">
      <div className="row between spaced-bottom">
        <h2 id="co-title">Perusahaan & workspace</h2>
        <Button variant="primary" onClick={() => setAdding(true)}>＋ Tambah perusahaan</Button>
      </div>
      <p className="banner spaced-bottom" role="note">
        Hanya Superadmin UAT (data dummy). Satu perusahaan = satu workspace, tanpa parent/child. Workspace baru dibuat tanpa anggota.
        Di produksi bagian ini ditolak sampai SSO dan peran admin pusat ditetapkan.
      </p>
      {error && <WriteAlert error={error} onReload={() => { setError(null); res.reload(); }} />}
      <DataTable caption="Perusahaan dan workspace" rows={list.items} rowKey={(c) => c.companyId}
        empty={{ title: 'Belum ada perusahaan', body: 'Tambahkan perusahaan pertama.' }}
        columns={[
          { key: 'name', header: 'Perusahaan', cell: (c) => c.companyName },
          { key: 'code', header: 'Workspace', cell: (c) => <><code>{c.workspaceCode}</code>{c.workspaceId === workspace.id ? ' (aktif dibuka)' : ''}</> },
          { key: 'members', header: 'Anggota', cell: (c) => c.memberCount },
          { key: 'status', header: 'Status', cell: (c) => <Badge tone={c.active ? 'ok' : 'neutral'}>{c.active ? 'Aktif' : 'Nonaktif'}</Badge> },
          {
            key: 'act', header: 'Aksi',
            cell: (c) => <Button onClick={() => { setError(null); setToggle(c); }} aria-label={`${c.active ? 'Nonaktifkan' : 'Aktifkan'} ${c.companyName}`}>
              {c.active ? 'Nonaktifkan' : 'Aktifkan'}</Button>,
          },
        ]} />
      {adding && (
        <CompanyDialog api={api} onClose={() => setAdding(false)}
          onDone={(c) => { res.set({ items: [...list.items, c] }); setAdding(false); onNotice(`${c.companyName} (${c.workspaceCode}) dibuat tanpa anggota. Tercatat di audit.`); }} />
      )}
      {toggle && (
        <ConfirmDialog title={`${toggle.active ? 'Nonaktifkan' : 'Aktifkan'} ${toggle.companyName}?`}
          confirmLabel={toggle.active ? 'Nonaktifkan' : 'Aktifkan'}
          impact={toggle.active
            ? `Workspace ${toggle.workspaceCode} tidak lagi dapat dibuka oleh anggotanya. Tiket, laporan dan histori tidak dihapus.${toggle.workspaceId === workspace.id ? ' Ini workspace yang sedang Anda buka.' : ''}`
            : `Workspace ${toggle.workspaceCode} dapat dibuka kembali oleh anggotanya yang aktif.`}
          onClose={() => setToggle(null)}
          onConfirm={async () => {
            try {
              const r = await api.setCompanyActive(toggle.companyId, { active: !toggle.active, version: toggle.version });
              replace(r.item);
              onNotice(`${r.item.companyName} ${r.item.active ? 'diaktifkan' : 'dinonaktifkan'}. Tercatat di audit.`);
            } catch (e) {
              setError(writeError(e, 'Status perusahaan'));
            }
            setToggle(null);
          }} />
      )}
    </section>
  );
}

function CompanyDialog({ api, onClose, onDone }: { api: Api; onClose: () => void; onDone: (c: WorkspaceSettings) => void }) {
  const [form, setForm] = useState({ name: '', code: '', timezone: 'Asia/Jakarta' as Timezone, reporterCode: '' });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const formId = useId();
  const valid = form.name.trim() !== '' && /^[A-Z0-9]{2,10}$/.test(form.code);
  async function submit() {
    if (busy || !valid) return;
    setBusy(true);
    setError(null);
    try {
      onDone((await api.createCompany({
        code: form.code, name: form.name.trim(), timezone: form.timezone, reporterCode: form.reporterCode.trim() || null,
      })).item);
    } catch (e) {
      setBusy(false);
      setError(writeError(e, 'Perusahaan'));
    }
  }
  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title="Tambah perusahaan & workspace" initialFocus="input"
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button type="submit" form={formId} variant="primary" disabled={busy || !valid} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan'}</Button>
        </>
      )}>
      {error && <WriteAlert error={error} />}
      <form id={formId} className="formgrid" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
        <TextField label="Nama perusahaan" required maxLength={200} value={form.name} disabled={busy}
          onChange={(e) => setForm({ ...form, name: e.target.value })} />
        <TextField label="Kode workspace" required maxLength={10} value={form.code} disabled={busy}
          hint="2–10 huruf besar/angka; tidak berubah setelah dibuat."
          error={error?.field === 'code' ? 'Kode sudah dipakai atau tidak valid.' : undefined}
          onChange={(e) => setForm({ ...form, code: e.target.value.toUpperCase() })} />
        <SelectField label="Zona waktu" value={form.timezone} disabled={busy}
          onChange={(e) => setForm({ ...form, timezone: e.target.value as Timezone })}>
          {Object.entries(TZ_LABEL).map(([v, l]) => <option key={v} value={v}>{l}</option>)}
        </SelectField>
        <TextField label="Kode pelapor" maxLength={40} value={form.reporterCode} disabled={busy}
          hint="Opsional; isi setelah kode terverifikasi." onChange={(e) => setForm({ ...form, reporterCode: e.target.value })} />
        <p className="full tiny muted">Empat prioritas baku dibuat otomatis. Nonaktif tidak menghapus tiket, laporan atau histori.</p>
      </form>
    </Dialog>
  );
}
