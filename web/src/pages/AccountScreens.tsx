import { useEffect, useRef, useState, type FormEvent, type ReactNode } from 'react';
import logoUrl from '../assets/logo-qash.svg';
import { ApiError, loginErrorText, rateLimitText, type Api } from '../api/client';
import type { Meta, Session } from '../api/types';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { TextField } from '../components/Field';
import { Popover } from '../components/Popover';
import { initials } from '../app/labels';
import {
  MIN_PASSWORD_LENGTH,
  passwordChangeError,
  validatePasswordChange,
  type PasswordErrors,
  type PasswordField,
} from '../app/account';

const MODE_LINKS = { sandbox: 'https://sandbox-itsm.scowork.my.id:8443/', operational: 'https://itsm.scowork.my.id:8443/' };
export function ModePicker({ sandbox }: { sandbox: boolean }) {
  return <div className="deployment-picker"><label htmlFor="deployment-mode">Lingkungan</label>
    <select id="deployment-mode" value={sandbox ? 'sandbox' : 'operational'} onChange={e => { window.location.href = MODE_LINKS[e.target.value as keyof typeof MODE_LINKS]; }}>
      <option value="operational">Operasional · data kerja</option><option value="sandbox">Sandbox · data latihan</option>
    </select><small>{sandbox ? 'Data latihan terpisah dari operasional.' : 'Database operasional terpisah dari sandbox.'}</small></div>;
}

/** A message carried to the login screen: why the person is back here. */
export interface LoginNotice { title: string; text: string }

/** Split layout of the owner reference (design/reference-owner-v1/01-login.png). */
function AuthLayout({ children }: { children: ReactNode }) {
  return (
    <div className="auth-screen">
      <section className="auth-hero" aria-label="Tentang aplikasi">
        <img className="auth-hero-logo" src={logoUrl} alt="" />
        <p className="auth-eyebrow">DECA · Customer Support</p>
        <p className="auth-headline">Satu tempat untuk setiap pengaduan merchant.</p>
        <p className="auth-lead">Tiket tercatat, penanganan terlacak, dan jejak audit tersimpan. Kanal masuk dan laporan operasional dalam satu alur kerja.</p>
        <ol className="auth-points">
          <li>Tiket bernomor dengan siklus resolve &amp; reopen</li>
          <li>Peran CS, Lead CS, dan grant terpisah</li>
          <li>Jejak audit append-only untuk setiap perubahan</li>
        </ol>
        <p className="auth-foot">Akses terbatas · aktivitas tercatat</p>
      </section>
      <main className="auth-main" id="main">
        <div className="auth-card">
          <div className="auth-brand">
            <img className="brand-logo" src={logoUrl} alt="Qash" />
            <span><strong>SERVICE DESK</strong><small>ITSM · akses internal</small></span>
          </div>
          {children}
        </div>
      </main>
    </div>
  );
}

/** US-1.1 login (password mode). Every refusal reads the same; the password is cleared after each try. */
export function LoginScreen({ api, meta, notice, onSuccess }: {
  api: Api;
  meta: Meta;
  notice: LoginNotice | null;
  onSuccess: (username: string) => void;
}) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState('');

  async function login(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setBusy(true);
    setError('');
    try {
      await api.login(username, password);
      setPassword('');
      onSuccess(username);
    } catch (err) {
      setPassword('');
      setError(loginErrorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
      <h1>Masuk ke workspace</h1>
      <p className="muted">Gunakan akun internal yang diberikan administrator.</p>
      {notice && (
        <div className="auth-notice" role="status">
          <strong>{notice.title}</strong>
          <span>{notice.text}</span>
        </div>
      )}
      <ModePicker sandbox={meta.environment === 'uat'} />
      {meta.accessMode === 'password' ? (
        <form className="auth-form" onSubmit={(e) => void login(e)}>
          <TextField label="Nama pengguna" name="username" autoComplete="username" required maxLength={254}
            value={username} onChange={(e) => setUsername(e.target.value)} disabled={busy} />
          <TextField label="Kata sandi" name="password" type="password" autoComplete="current-password" required
            maxLength={1024} value={password} onChange={(e) => setPassword(e.target.value)} disabled={busy} />
          {error && <p role="alert" className="auth-error">{error}</p>}
          <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Memverifikasi…' : 'Masuk'}</Button>
        </form>
      ) : <p>Akses SSO belum dikonfigurasi.</p>}
      <p className="tiny muted auth-help">Belum punya akses? Hubungi administrator untuk pembuatan akun.</p>
      <p className="tiny muted">{meta.environment === 'uat' ? 'Sandbox — gunakan data latihan saja.' : 'Operasional — akses sesuai perusahaan dan peran.'}</p>
    </AuthLayout>
  );
}

/**
 * US-1.4 forced change: the only screen while `mustChangePassword` is set (the server refuses the
 * rest with 403). Success keeps this session; the app then loads the workspace as usual.
 */
export function ChangePasswordScreen({ api, session, username, onChanged, onLogout }: {
  api: Api;
  session: Session;
  username: string | null;
  onChanged: () => void;
  onLogout: () => void;
}) {
  const [currentPassword, setCurrent] = useState('');
  const [newPassword, setNew] = useState('');
  const [confirmPassword, setConfirm] = useState('');
  const [errors, setErrors] = useState<PasswordErrors>({});
  const [formError, setFormError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // The first refused field. Focus moves once the inputs are enabled again, so a field refused
  // by the server (answered while the form was busy and disabled) still receives it.
  const [focusField, setFocusField] = useState<PasswordField | null>(null);
  const formRef = useRef<HTMLFormElement>(null);

  useEffect(() => {
    if (busy || !focusField) return;
    formRef.current?.querySelector<HTMLInputElement>(`input[name="${focusField}"]`)?.focus();
    setFocusField(null);
  }, [busy, focusField]);

  const firstRefused = (found: PasswordErrors) =>
    (['currentPassword', 'newPassword', 'confirmPassword'] as const).find((f) => found[f]) ?? null;

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (busy) return;
    setFormError(null);
    const found = validatePasswordChange({ currentPassword, newPassword, confirmPassword, username });
    setErrors(found);
    if (Object.keys(found).length > 0) { setFocusField(firstRefused(found)); return; }
    setBusy(true);
    try {
      await api.changePassword(currentPassword, newPassword);
      setCurrent(''); setNew(''); setConfirm('');
      onChanged();
    } catch (err) {
      const { fields, form } = passwordChangeError(err);
      if (fields.currentPassword) setCurrent('');
      setErrors(fields);
      setFormError(form);
      setFocusField(firstRefused(fields));
    } finally {
      setBusy(false);
    }
  }

  return (
    <AuthLayout>
      <h1>Ganti kata sandi</h1>
      <p className="muted">Halo, {session.user.displayName}. Kata sandi Anda bersifat sementara. Buat kata sandi baru sebelum membuka workspace.</p>
      <form ref={formRef} className="auth-form" onSubmit={(e) => void submit(e)} noValidate>
        <TextField label="Kata sandi saat ini" name="currentPassword" type="password" autoComplete="current-password"
          maxLength={1024} value={currentPassword} onChange={(e) => setCurrent(e.target.value)} disabled={busy}
          error={errors.currentPassword} />
        <TextField label="Kata sandi baru" name="newPassword" type="password" autoComplete="new-password"
          maxLength={1024} value={newPassword} onChange={(e) => setNew(e.target.value)} disabled={busy}
          hint={`Minimal ${MIN_PASSWORD_LENGTH} karakter, tidak sama dengan nama pengguna atau kata sandi saat ini.`}
          error={errors.newPassword} />
        <TextField label="Ulangi kata sandi baru" name="confirmPassword" type="password" autoComplete="new-password"
          maxLength={1024} value={confirmPassword} onChange={(e) => setConfirm(e.target.value)} disabled={busy}
          error={errors.confirmPassword} />
        {formError && <p role="alert" className="auth-error">{formError}</p>}
        <Button type="submit" variant="primary" disabled={busy}>{busy ? 'Menyimpan…' : 'Simpan kata sandi baru'}</Button>
        <Button variant="ghost" disabled={busy} onClick={onLogout}>Keluar</Button>
      </form>
      <p className="tiny muted">Sesi lain milik akun ini akan diakhiri setelah kata sandi diganti.</p>
    </AuthLayout>
  );
}

/** Avatar menu in the top bar: who is signed in, and (password mode) US-1.6 logout everywhere. */
export function AccountMenu({ api, session, passwordMode, onLoggedOutAll }: {
  api: Api;
  session: Session;
  passwordMode: boolean;
  onLoggedOutAll: () => void;
}) {
  const [confirming, setConfirming] = useState(false);
  return (
    <>
      <Popover className="account-menu" label="Menu akun"
        trigger={<span className="avatar top-avatar" aria-hidden="true">{initials(session.user.displayName)}</span>}>
        {(close) => (
          <div className="account-menu-body">
            <span className="sr-only">Akun: </span><strong>{session.user.displayName}</strong>
            <p className="tiny muted">{session.uatSuperadmin ? 'Superadmin UAT · data dummy' : 'Pengguna'}</p>
            {passwordMode && (
              <Button onClick={() => { close(); setConfirming(true); }}>Keluar dari semua perangkat</Button>
            )}
          </div>
        )}
      </Popover>
      <LogoutAllDialog api={api} open={confirming} onClose={() => setConfirming(false)} onDone={onLoggedOutAll} />
    </>
  );
}

export function LogoutAllDialog({ api, open, onClose, onDone }: {
  api: Api;
  open: boolean;
  onClose: () => void;
  onDone: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const close = () => { if (!busy) { setError(null); onClose(); } };

  async function confirm() {
    setBusy(true);
    setError(null);
    try {
      await api.logoutAll();
      onDone();
    } catch (e) {
      if (e instanceof ApiError && e.status === 401) return; // session already ended: the app shows the login
      setError(e instanceof ApiError && e.status === 429 ? rateLimitText(e)
        : 'Belum dapat keluar dari semua perangkat. Coba lagi.');
    } finally {
      setBusy(false);
    }
  }

  return (
    <Dialog open={open} onClose={close} title="Keluar dari semua perangkat?" initialFocus=".logout-all-cancel"
      footer={open && (
        <>
          <Button className="logout-all-cancel" onClick={close} disabled={busy}>Batal</Button>
          <Button variant="primary" onClick={() => void confirm()} disabled={busy}>
            {busy ? 'Memproses…' : 'Keluar dari semua perangkat'}
          </Button>
        </>
      )}>
      <p>Semua sesi akun ini diakhiri, termasuk sesi di perangkat ini. Anda perlu masuk kembali.</p>
      <p className="tiny muted">Draf tiket yang belum terkirim di tab ini akan hilang.</p>
      {error && <p role="alert" className="auth-error">{error}</p>}
    </Dialog>
  );
}
