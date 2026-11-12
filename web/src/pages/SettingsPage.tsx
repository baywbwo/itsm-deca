import { useId, useState } from 'react';
import { ApiError, type Api } from '../api/client';
import type {
  CompanyList, GrantCode, Member, MemberList, SettingsBody, Timezone,
  Workspace, WorkspaceSettings,
} from '../api/types';
import { BRAND_PRESETS } from '../app/brand';
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

type TabId = 'workspace' | 'members' | 'companies';

/**
 * O07/O09 Pengaturan. Every tab reads its own server answer; edit controls appear only when the
 * server says so (`canEdit`/`canManage`), and the server re-checks each write (default deny).
 */
export function SettingsPage({ api, workspace, uat, permissions, onNotice, onWorkspacesChanged = () => undefined }: {
  api: Api;
  workspace: Workspace;
  uat: boolean;
  permissions: readonly string[];
  onNotice: (text: string) => void;
  /** A save changed what the workspace switcher lists (name, new company, active). */
  onWorkspacesChanged?: () => void;
}) {
  const tabs: { id: TabId; label: string }[] = [
    ...(permissions.includes('catalog.read') ? [{ id: 'workspace' as const, label: 'Workspace' }] : []),
    ...(permissions.includes('member.read') ? [{ id: 'members' as const, label: 'Anggota' }] : []),
    ...(uat ? [{ id: 'companies' as const, label: 'Perusahaan' }] : []),
  ];
  const [tab, setTab] = useState<TabId>(tabs[0]?.id ?? 'workspace');
  const panelId = useId();
  if (tabs.length === 0) return <ForbiddenState />;
  return (
    <>
      <PageHeader eyebrow={workspace.code} title="Pengaturan"
        description={`Kelola profil perusahaan dan anggota ${workspace.code}.`} />
      <Tabs tabs={tabs} value={tab} onChange={setTab} label="Bagian pengaturan" panelId={panelId} />
      <div id={panelId} role="tabpanel" aria-labelledby={`${panelId}-tab-${tab}`}>
        {tab === 'workspace' && <WorkspaceTab api={api} workspace={workspace} onNotice={onNotice} onChanged={onWorkspacesChanged} />}
        {tab === 'members' && <MembersTab api={api} workspace={workspace} onNotice={onNotice} />}
        {tab === 'companies' && <CompaniesTab api={api} workspace={workspace} onNotice={onNotice} onChanged={onWorkspacesChanged} />}
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

function WorkspaceTab({ api, workspace, onNotice, onChanged }: {
  api: Api; workspace: Workspace; onNotice: (t: string) => void; onChanged: () => void;
}) {
  const res = useLoad((s) => api.getSettings(workspace.id, s), workspace.id);
  if (res.error) return <LoadFailure error={res.error} what="Profil workspace" onRetry={res.reload} />;
  if (!res.data) return <LoadingState label="Memuat profil workspace…" />;
  return <WorkspaceForm key={res.data.settings.version} api={api} body={res.data} onReload={res.reload}
    onSaved={(b) => { res.set(b); onChanged(); onNotice('Profil workspace tersimpan dan tercatat di audit.'); }} />;
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
    ...(s.previousCodes.length > 0 ? [['Kode sebelumnya', s.previousCodes.join(', ')] as [string, string]] : []),
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

// ---------------------------------------------------------------- Companies (UAT Superadmin)

function CompaniesTab({ api, workspace, onNotice, onChanged }: {
  api: Api; workspace: Workspace; onNotice: (t: string) => void; onChanged: () => void;
}) {
  const res = useLoad((s) => api.listCompanies(s), 'companies');
  const [adding, setAdding] = useState(false);
  const [toggle, setToggle] = useState<WorkspaceSettings | null>(null);
  const [editId, setEditId] = useState<string | null>(null);
  const [error, setError] = useState<WriteError | null>(null);
  if (res.error) return <LoadFailure error={res.error} what="Daftar perusahaan" onRetry={res.reload} />;
  const list = res.data;
  if (!list) return <LoadingState label="Memuat perusahaan…" />;
  const replace = (c: WorkspaceSettings) => {
    res.set({ items: list.items.map((x) => (x.companyId === c.companyId ? c : x)) });
    onChanged();
  };
  const editing = list.items.find((c) => c.companyId === editId) ?? null;
  return (
    <section aria-labelledby="co-title">
      <div className="row between spaced-bottom">
        <h2 id="co-title">Perusahaan & workspace</h2>
        <Button variant="primary" onClick={() => setAdding(true)}>＋ Tambah perusahaan</Button>
      </div>
      <p className="banner spaced-bottom" role="note">
        Hanya Superadmin. Satu perusahaan = satu workspace, tanpa parent/child. Workspace baru dibuat tanpa anggota.
      </p>
      {error && <WriteAlert error={error} onReload={() => { setError(null); res.reload(); }} />}
      <DataTable caption="Perusahaan dan workspace" rows={list.items} rowKey={(c) => c.companyId}
        empty={{ title: 'Belum ada perusahaan', body: 'Tambahkan perusahaan pertama.' }}
        columns={[
          { key: 'name', header: 'Perusahaan', cell: (c) => c.companyName },
          { key: 'code', header: 'Workspace', cell: (c) => <><code>{c.workspaceCode}</code> {c.workspaceName}{c.workspaceId === workspace.id ? ' (aktif dibuka)' : ''}</> },
          { key: 'brand', header: 'Branding', cell: (c) => (c.brandKey ? BRAND_PRESETS[c.brandKey].label : 'Netral (belum diatur)') },
          { key: 'members', header: 'Anggota', cell: (c) => c.memberCount },
          { key: 'status', header: 'Status', cell: (c) => <Badge tone={c.active ? 'ok' : 'neutral'}>{c.active ? 'Aktif' : 'Nonaktif'}</Badge> },
          {
            key: 'act', header: 'Aksi',
            cell: (c) => (
              <div className="row">
                <Button onClick={() => { setError(null); setEditId(c.companyId); }} aria-label={`Edit ${c.companyName}`}>Edit</Button>
                <Button onClick={() => { setError(null); setToggle(c); }} aria-label={`${c.active ? 'Nonaktifkan' : 'Aktifkan'} ${c.companyName}`}>
                  {c.active ? 'Nonaktifkan' : 'Aktifkan'}</Button>
              </div>
            ),
          },
        ]} />
      {adding && (
        <CompanyDialog api={api} onClose={() => setAdding(false)}
          onDone={(c) => { res.set({ items: [...list.items, c] }); setAdding(false); onChanged(); onNotice(`${c.companyName} (${c.workspaceCode}) dibuat tanpa anggota. Tercatat di audit.`); }} />
      )}
      {editing && (
        <CompanyEditDialog api={api} company={editing} onClose={() => setEditId(null)}
          onRefreshed={(l) => { res.set(l); onChanged(); }}
          onDone={(c, changed) => {
            replace(c);
            setEditId(null);
            onNotice(changed ? `${c.companyName} (${c.workspaceCode}) tersimpan. Tercatat di audit.` : 'Tidak ada perubahan; tidak ada yang disimpan.');
          }} />
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

/**
 * Edits an existing company + its workspace, prefilled with the listed values. The business
 * code is editable (D17): new tickets use the new code, tickets already issued keep their
 * number, and every earlier code stays reserved for this company. The server and database
 * validate format and uniqueness; uppercasing here is only a convenience. A 409 keeps the
 * input; reloading fetches the latest version so the same input can be saved again.
 */
function CompanyEditDialog({ api, company, onClose, onDone, onRefreshed }: {
  api: Api;
  company: WorkspaceSettings;
  onClose: () => void;
  onDone: (c: WorkspaceSettings, changed: boolean) => void;
  onRefreshed: (list: CompanyList) => void;
}) {
  const [form, setForm] = useState({
    code: company.workspaceCode,
    companyName: company.companyName, workspaceName: company.workspaceName,
    timezone: company.timezone as Timezone, reporterCode: company.reporterCode ?? '',
    brandKey: company.brandKey ?? '', brandName: company.brandName ?? '',
  });
  const preset = form.brandKey ? BRAND_PRESETS[form.brandKey as keyof typeof BRAND_PRESETS] : null;
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const [refreshed, setRefreshed] = useState(false);
  const formId = useId();
  const codeOk = CODE_PATTERN.test(form.code);
  const codeChanged = form.code !== company.workspaceCode;
  const valid = form.companyName.trim() !== '' && form.workspaceName.trim() !== '' && codeOk;
  async function submit() {
    if (busy || !valid) return;
    setBusy(true);
    setError(null);
    try {
      const r = await api.updateCompany(company.companyId, {
        code: form.code,
        companyName: form.companyName.trim(), workspaceName: form.workspaceName.trim(), timezone: form.timezone,
        reporterCode: form.reporterCode.trim() || null,
        brandKey: preset ? preset.key : null, brandName: form.brandName.trim() || null, version: company.version,
      });
      onDone(r.item, r.auditEventId !== null);
    } catch (e) {
      setBusy(false);
      setRefreshed(false);
      setError(writeError(e, 'Perusahaan'));
    }
  }
  async function reload() {
    setBusy(true);
    try {
      onRefreshed(await api.listCompanies());
      setError(null);
      setRefreshed(true);
    } catch (e) {
      setError(writeError(e, 'Daftar perusahaan'));
    }
    setBusy(false);
  }
  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title={`Edit ${company.companyName}`} initialFocus="input"
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button type="submit" form={formId} variant="primary" disabled={busy || !valid} aria-busy={busy}>{busy ? 'Menyimpan…' : 'Simpan perubahan'}</Button>
        </>
      )}>
      {error && <WriteAlert error={error} onReload={() => { void reload(); }} />}
      {refreshed && (
        <p className="banner spaced-bottom" role="status">
          Data terbaru dimuat. Isian Anda tetap; periksa lalu simpan lagi.
        </p>
      )}
      <form id={formId} className="formgrid" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
        <TextField label="Nama perusahaan" required maxLength={200} value={form.companyName} disabled={busy}
          error={error?.field === 'companyName' ? 'Isi nama perusahaan (maks. 200 karakter).' : undefined}
          onChange={(e) => setForm({ ...form, companyName: e.target.value })} />
        <TextField label="Nama workspace" required maxLength={200} value={form.workspaceName} disabled={busy}
          hint="Tampil di pemilih workspace."
          error={error?.field === 'workspaceName' ? 'Isi nama workspace (maks. 200 karakter).' : undefined}
          onChange={(e) => setForm({ ...form, workspaceName: e.target.value })} />
        <TextField label="Kode workspace" required maxLength={10} value={form.code} disabled={busy}
          autoCapitalize="characters" spellCheck={false}
          hint="2–10 huruf besar/angka. Dipakai untuk nomor tiket BARU; nomor tiket lama tidak berubah."
          error={error?.field === 'code'
            ? 'Kode sudah/pernah dipakai perusahaan lain atau formatnya tidak valid.'
            : !codeOk && form.code !== '' ? 'Gunakan 2–10 huruf besar A–Z atau angka.' : undefined}
          onChange={(e) => setForm({ ...form, code: e.target.value.toUpperCase().replace(/\s/g, '') })} />
        <div className="full tiny" data-testid="code-impact" role="status" aria-live="polite">
          {codeChanged && codeOk ? (
            <p className="banner">
              Tiket baru akan bernomor <strong>{form.code}-…</strong> dan melanjutkan urutan yang ada.
              Tiket yang sudah terbit tetap memakai nomornya (mis. <strong>{company.workspaceCode}-1</strong>)
              di daftar, detail, pencarian dan ekspor. Kode <strong>{company.workspaceCode}</strong> tetap dicadangkan
              untuk perusahaan ini. Kode perusahaan ikut berubah; kode pelapor tidak.
            </p>
          ) : (
            <p className="muted">
              Kode saat ini {company.workspaceCode}
              {company.previousCodes.length > 0 && <> · kode sebelumnya: {company.previousCodes.join(', ')} (tetap dicadangkan)</>}.
            </p>
          )}
        </div>
        <SelectField label="Zona waktu" value={form.timezone} disabled={busy}
          onChange={(e) => setForm({ ...form, timezone: e.target.value as Timezone })}>
          {Object.entries(TZ_LABEL).map(([v, l]) => <option key={v} value={v}>{l}</option>)}
        </SelectField>
        <TextField label="Kode pelapor" maxLength={40} value={form.reporterCode} disabled={busy}
          hint="Opsional; isi setelah kode terverifikasi."
          error={error?.field === 'reporterCode' ? 'Periksa kode pelapor.' : undefined}
          onChange={(e) => setForm({ ...form, reporterCode: e.target.value })} />
        <SelectField label="Branding" value={form.brandKey} disabled={busy}
          hint="Logo dan warna aplikasi saat workspace ini dibuka. Hanya preset bawaan aplikasi."
          error={error?.field === 'brandKey' ? 'Pilih salah satu preset.' : undefined}
          onChange={(e) => setForm({ ...form, brandKey: e.target.value })}>
          <option value="">Netral (tanpa logo)</option>
          {Object.values(BRAND_PRESETS).map((p) => <option key={p.key} value={p.key}>{p.label}</option>)}
        </SelectField>
        <TextField label="Nama tampilan" maxLength={60} value={form.brandName} disabled={busy}
          hint="Opsional; tampil di bawah logo dan judul tab. Kosong = nama perusahaan."
          error={error?.field === 'brandName' ? 'Maks. 60 karakter.' : undefined}
          onChange={(e) => setForm({ ...form, brandName: e.target.value })} />
        <div className="full brand-preview" data-testid="brand-preview" aria-label="Pratinjau branding">
          {preset
            ? <img className="brand-logo" src={preset.logo} alt={`Logo ${preset.logoAlt}`} />
            : <span className="brand-mark">ITSM</span>}
          <span>
            <span className="brand-name">{form.brandName.trim() || form.companyName.trim() || company.companyName}</span>
            <span className="tiny muted"> · {preset ? preset.label : 'Netral'}</span>
          </span>
        </div>
        <p className="full tiny muted">Hanya profil yang berubah. Data, nomor tiket yang sudah terbit, laporan dan histori tidak dipindah atau dinomori ulang; perubahan tercatat di audit.</p>
      </form>
    </Dialog>
  );
}

/** Server rule for workspace codes; the API and the database enforce it too. */
const CODE_PATTERN = /^[A-Z0-9]{2,10}$/;

function CompanyDialog({ api, onClose, onDone }: { api: Api; onClose: () => void; onDone: (c: WorkspaceSettings) => void }) {
  const [form, setForm] = useState({ name: '', code: '', timezone: 'Asia/Jakarta' as Timezone, reporterCode: '' });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<WriteError | null>(null);
  const formId = useId();
  const valid = form.name.trim() !== '' && CODE_PATTERN.test(form.code);
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
