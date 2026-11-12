import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import { createApi } from '../api/client';
import type { Meta, Session } from '../api/types';
import { SESSION_ENDED_TEXT, SESSION_ENDED_TITLE } from '../app/account';
import { ChangePasswordScreen, LoginScreen } from './AccountScreens';

// CP3a markup (server render, no DOM): labelled fields, the "Sesi berakhir" reason on the login,
// and a change screen that offers nothing but the change and logout.
const api = createApi(async () => { throw new Error('no request expected while rendering'); });
const meta = { environment: 'production', accessMode: 'password', banner: null, uat: null } as Meta;
const session = { user: { id: 'u-1', displayName: 'Sari Wulandari' }, banner: null, uatSuperadmin: false, mustChangePassword: true } as Session;
const noop = () => undefined;

describe('CP3a account screens', () => {
  it('login: labelled username/password fields, password autocomplete, no reason by default', () => {
    const html = renderToStaticMarkup(<LoginScreen api={api} meta={meta} notice={null} onSuccess={noop} />);
    expect(html).toContain('Masuk ke workspace');
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
