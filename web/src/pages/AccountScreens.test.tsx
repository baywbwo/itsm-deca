import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { createApi } from '../api/client';
import type { Meta, Session } from '../api/types';
import { PASSWORD_CHANGED_TEXT, SESSION_ENDED_TEXT, SESSION_ENDED_TITLE } from '../app/account';
import { accountRole, ChangePasswordDialog, ChangePasswordScreen, LoginScreen } from './AccountScreens';
import { TemporaryPasswordDialog } from './UsersPage';

// CP3a markup (server render, no DOM): labelled fields, the "Sesi berakhir" reason on the login,
// and a change screen that offers nothing but the change and logout.
const api = createApi(async () => { throw new Error('no request expected while rendering'); });
const meta = { environment: 'production', accessMode: 'password', banner: null, uat: null } as Meta;
const session = { user: { id: 'u-1', displayName: 'Sari Wulandari' }, banner: null, uatSuperadmin: false, mustChangePassword: true } as Session;
const noop = () => undefined;

describe('CP3a account screens', () => {
  it('login: labelled username/password fields, password autocomplete, no reason by default', () => {
    const html = renderToStaticMarkup(<LoginScreen api={api} meta={meta} notice={null} onSuccess={noop} />);
    expect(html).toContain('Masuk ke ITSM');
    expect(html).toMatch(/<label for="([^"]+)">Nama pengguna<\/label><input id="\1"[^>]*autoComplete="username"/i);
    expect(html).toMatch(/<label for="([^"]+)">Kata sandi<\/label><input id="\1"[^>]*type="password"[^>]*autoComplete="current-password"/i);
    expect(html).not.toContain(SESSION_ENDED_TITLE);
    expect(html).not.toContain('role="alert"');
  });

  it('login after an expired session says why, as a polite status', () => {
    const html = renderToStaticMarkup(<LoginScreen api={api} meta={meta}
      notice={{ title: SESSION_ENDED_TITLE, text: SESSION_ENDED_TEXT }} onSuccess={noop} />);
    expect(html).toMatch(new RegExp(`role="status"><strong>${SESSION_ENDED_TITLE}</strong><span>${SESSION_ENDED_TEXT}`));
  });

  it('forced change: three password fields with the rule as hint, save and logout only', () => {
    const html = renderToStaticMarkup(<ChangePasswordScreen api={api} session={session} username="sari"
      onChanged={noop} onLogout={noop} />);
    expect(html).toContain('Ganti kata sandi');
    expect(html.match(/type="password"/g)).toHaveLength(3);
    expect(html.match(/autoComplete="new-password"/gi)).toHaveLength(2);
    expect(html).toMatch(/aria-describedby="[^"]+-hint"/);
    expect(html).toContain('Minimal 12 karakter');
    const buttons = [...html.matchAll(/<button[^>]*>([^<]*)<\/button>/g)].map((m) => m[1]);
    expect(buttons).toEqual(['Simpan kata sandi baru', 'Keluar']);
    expect(html).not.toMatch(/href="#\//);
  });
});

describe('CP3b account and user administration markup', () => {
  it('voluntary change dialog: same three fields, save and cancel, says this session stays', () => {
    const html = renderToStaticMarkup(<ChangePasswordDialog api={api} open username="sari" onClose={noop} onDone={noop} />);
    expect(html.match(/type="password"/g)).toHaveLength(3);
    expect(html).toContain('Anda tetap masuk di perangkat ini');
    const buttons = [...html.matchAll(/<button[^>]*>([^<]*)<\/button>/g)].map((m) => m[1]);
    expect(buttons).toContain('Simpan kata sandi baru');
    expect(buttons).toContain('Batal');
    expect(PASSWORD_CHANGED_TEXT).toContain('sesi lain');
  });

  it('closed dialog renders no password field', () => {
    const html = renderToStaticMarkup(<ChangePasswordDialog api={api} open={false} username={null} onClose={noop} onDone={noop} />);
    expect(html).not.toContain('type="password"');
  });

  it('temporary password: labelled read-only value, copy button, cannot-see-again warning', () => {
    const html = renderToStaticMarkup(<TemporaryPasswordDialog onClose={noop}
      secret={{ title: 'Pengguna dibuat', who: 'Budi Santoso', username: 'budi.santoso', password: 'contoh-sandi-sementara-0001' }} />);
    expect(html).toMatch(/for="temp-password">Kata sandi sementara<\/label><input id="temp-password"[^>]*readOnly=""[^>]*value="contoh-sandi-sementara-0001"/i);
    expect(html).toContain('>Salin</button>');
    expect(html).toContain('Tidak dapat dilihat lagi');
    expect(html).toContain('Saya sudah menyimpannya');
  });
});

// Owner 2026-11-12: the app presents as production; no UAT/sandbox banner, switch or label in the UI.
describe('CP-UI1b no environment banner', () => {
  it('login on a UAT deployment shows no sandbox/UAT text or environment switch', () => {
    const uatMeta = { ...meta, environment: 'uat' } as Meta;
    const html = renderToStaticMarkup(<LoginScreen api={api} meta={uatMeta} notice={null} onSuccess={noop} />);
    expect(html).not.toMatch(/\b(sandbox|uat|latihan|lingkungan|dummy)\b/i);
    expect(html).not.toContain('<select');
  });

  it('role line of the UAT superadmin carries no UAT/dummy label', () => {
    expect(accountRole({ ...session, uatSuperadmin: true })).toBe('Superadmin');
    expect(accountRole({ ...session, platformAdmin: true })).toBe('Admin platform');
  });
});
