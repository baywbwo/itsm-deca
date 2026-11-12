import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { ApiError, createApi } from '../api/client';
import type { Meta } from '../api/types';
import { allowedPages } from '../app/routes';
import { AwaitingAccessScreen, LoginScreen, SsoLoginForm } from './AccountScreens';
import { ssoWriteError } from './SsoPage';

const api = createApi(async () => { throw new Error('no request expected while rendering'); });
const meta = (sso: boolean) => ({ environment: 'production', accessMode: 'password', banner: null, uat: null, sso }) as Meta;
const noop = () => undefined;

describe('CP5c login with SSO', () => {
  it('meta.sso shows a company-code entry next to the password form; no company list is published', () => {
    const html = renderToStaticMarkup(<LoginScreen api={api} meta={meta(true)} notice={null} onSuccess={noop} />);
    expect(html).toContain('Masuk dengan SSO');
    expect(html).toMatch(/<label for="([^"]+)">Kode perusahaan<\/label><input id="\1"[^>]*maxLength="10"/i);
    expect(html).toContain('type="password"');
    expect(html).not.toContain('<select');
  });

  it('without meta.sso (or an old server without the field) there is no SSO entry', () => {
    expect(renderToStaticMarkup(<LoginScreen api={api} meta={meta(false)} notice={null} onSuccess={noop} />)).not.toContain('Masuk dengan SSO');
    const old = { environment: 'production', accessMode: 'password', banner: null, uat: null } as Meta;
    expect(renderToStaticMarkup(<LoginScreen api={api} meta={old} notice={null} onSuccess={noop} />)).not.toContain('Kode perusahaan');
  });

  it('a failed SSO round trip shows its reason as a polite status', () => {
    const html = renderToStaticMarkup(<LoginScreen api={api} meta={meta(true)}
      notice={{ title: 'Masuk dengan SSO gagal', text: 'Akun kantor Anda belum terhubung ke ITSM.' }} onSuccess={noop} />);
    expect(html).toMatch(/role="status"><strong>Masuk dengan SSO gagal<\/strong><span>Akun kantor Anda belum terhubung/);
  });

  it('standalone form renders the submit button enabled', () => {
    const html = renderToStaticMarkup(<SsoLoginForm disabled={false} go={noop} />);
    expect(html).toMatch(/<button[^>]*type="submit"[^>]*>Masuk dengan SSO<\/button>/);
    expect(html).not.toMatch(/<button[^>]*disabled/);
  });

  it('awaiting access: names the account, offers reload and logout only', () => {
    const session = { user: { id: 'u', displayName: 'Rina' } } as Parameters<typeof AwaitingAccessScreen>[0]['session'];
    const html = renderToStaticMarkup(<AwaitingAccessScreen session={session} onRetry={noop} onLogout={noop} />);
    expect(html).toContain('Menunggu akses');
    expect(html).toContain('Rina');
    const buttons = [...html.matchAll(/<button[^>]*>([^<]*)<\/button>/g)].map((m) => m[1]);
    expect(buttons).toEqual(['Muat ulang', 'Keluar']);
  });
});

describe('CP5c SSO administration page', () => {
  it('"Login SSO" is a platform-admin page only', () => {
    expect(allowedPages(['admin.settings', 'admin.members'], false, false).some((p) => p.id === 'sso')).toBe(false);
    const admin = allowedPages([], false, true).find((p) => p.id === 'sso');
    expect(admin?.label).toBe('Login SSO');
    expect(admin?.admin).toBe(true);
  });

  it('server refusals read as their meaning, with the request id', () => {
    const e = (status: number, code: ConstructorParameters<typeof ApiError>[1], field: string | null = null) =>
      new ApiError(status, code, 'msg', 'req-1', field);
    expect(ssoWriteError(e(409, 'idp_not_ready'), 'Status SSO').title).toBe('Belum siap diaktifkan');
    expect(ssoWriteError(e(409, 'subject_linked'), 'x').title).toBe('Subject sudah terhubung');
    expect(ssoWriteError(e(409, 'user_linked'), 'x').title).toBe('Akun sudah terhubung');
    expect(ssoWriteError(e(409, 'user_in_other_company'), 'x').body).toContain('satu akun hanya untuk satu perusahaan');
    expect(ssoWriteError(e(503, 'keyring_missing'), 'x').body).toContain('ITSM_KEYRING');
    const field = ssoWriteError(e(422, 'validation_failed', 'issuer'), 'x');
    expect(field.field).toBe('issuer');
    expect(field.body).toContain('https');
    expect(field.body).toContain('req-1');
    const stale = ssoWriteError(e(409, 'version_conflict'), 'Pengaturan SSO');
    expect(stale.conflict).toBe(true);
  });
});
