import { readFileSync } from 'node:fs';
import { renderToStaticMarkup } from 'react-dom/server';
import { describe, expect, it } from 'vitest';
import type { Workspace } from '../api/types';
import { CompanyCrumb, DrawerCompany } from './App';

// CP5a: the company comes from the account (SSO per company); the header and the mobile drawer
// name it as text. There is no workspace dropdown and no switching.
const dob = {
  id: 'a1e8dd4a-a301-42f1-ac2a-58db5c57811b', code: 'DOB', name: 'PT DOB (dummy)', timezone: 'Asia/Jakarta',
  companyId: 'c1', version: 1, brandKey: null, brandName: 'DOB', myRole: 'cs',
} as Workspace;

describe('CP5a header without a workspace switcher', () => {
  it('names the company as plain text next to the page, no select', () => {
    const html = renderToStaticMarkup(<CompanyCrumb company={dob} page="Tiket" />);
    expect(html).not.toMatch(/<select|role="combobox"|<option/);
    expect(html).toContain('data-testid="crumb-company">DOB</span>');
    expect(html).toContain('<b>Tiket</b>');
  });

  it('falls back to the company name and shows only the page while loading', () => {
    expect(renderToStaticMarkup(<CompanyCrumb company={{ ...dob, brandName: '' }} page="Dasbor" />))
      .toContain('>PT DOB (dummy)</span>');
    const loading = renderToStaticMarkup(<CompanyCrumb company={null} page="Dasbor" />);
    expect(loading).not.toContain('crumb-company');
    expect(loading).not.toContain('crumb-sep');
  });

  it('drawer shows the company as a label, not a field', () => {
    const html = renderToStaticMarkup(<DrawerCompany company={dob} />);
    expect(html).not.toMatch(/<select|<input|<label/);
    expect(html).toContain('Perusahaan');
    expect(renderToStaticMarkup(<DrawerCompany company={null} />)).toBe('');
  });

  it('the app shell renders no workspace select anywhere', () => {
    const src = readFileSync(new URL('./App.tsx', import.meta.url), 'utf8');
    expect(src).not.toMatch(/<select|switchWorkspace|pendingSwitch|crumb-ws"[^>]*onChange/);
  });
});
