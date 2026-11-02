/// <reference types="node" />
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { isWorkspace } from '../api/guards';
import type { Workspace } from '../api/types';
import { BRAND_PRESETS, NEUTRAL, activeBrand, contrast, pageTitle } from './brand';

const ws: Workspace = {
  id: 'a1e8dd4a-a301-42f1-ac2a-58db5c57811b', code: 'DUMA', name: 'Workspace A (dummy)', timezone: 'Asia/Jakarta',
  companyId: 'c1', version: 3, myRole: 'cs', brandKey: 'deca', brandName: 'PT Dummy Alfa (uji)',
};

describe('D15 company branding', () => {
  it('resolves the preset of the listed workspace, neutral without one', () => {
    expect(activeBrand(ws)).toMatchObject({ key: 'deca', name: 'PT Dummy Alfa (uji)', preset: { logoAlt: 'Qash' } });
    expect(activeBrand({ ...ws, brandKey: 'ajr' }).preset?.logoAlt).toBe('AJR');
    expect(activeBrand({ ...ws, brandKey: null })).toMatchObject({ key: null, preset: null, name: 'PT Dummy Alfa (uji)' });
    expect(activeBrand(null)).toEqual({ key: null, name: '', preset: null });
    expect(pageTitle(activeBrand(ws))).toBe('PT Dummy Alfa (uji) · ITSM');
    expect(pageTitle(activeBrand(null))).toBe('ITSM');
  });

  it('accepts only the bundled preset keys from the server', () => {
    expect(isWorkspace(ws)).toBe(true);
    expect(isWorkspace({ ...ws, brandKey: null })).toBe(true);
    for (const bad of ['https://example.com/logo.png', '../logo', 'DECA', 'qash', '', undefined]) {
      expect(isWorkspace({ ...ws, brandKey: bad })).toBe(false);
    }
    expect(isWorkspace({ ...ws, brandName: null })).toBe(false);
  });

  it('keeps text and buttons readable in every theme (WCAG AA 4.5:1)', () => {
    const ink = '#18261e';
    for (const t of [...Object.values(BRAND_PRESETS), NEUTRAL]) {
      expect(contrast('#ffffff', t.accent)).toBeGreaterThanOrEqual(4.5);
      expect(contrast('#ffffff', t.accentHover)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(ink, t.soft)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(t.accent, t.soft)).toBeGreaterThanOrEqual(4.5);
    }
  });

  it('the stylesheet carries the same preset colours as the registry', () => {
    const css = readFileSync(new URL('../styles/app.css', import.meta.url), 'utf8').toLowerCase();
    for (const p of Object.values(BRAND_PRESETS)) {
      const block = css.slice(css.indexOf(`:root[data-brand='${p.key}']`));
      const rule = block.slice(0, block.indexOf('}'));
      expect(rule).toContain(`--accent: ${p.accent}`);
      expect(rule).toContain(`--accent-hover: ${p.accentHover}`);
      expect(rule).toContain(`--soft: ${p.soft}`);
    }
    expect(css).toContain(`--accent: ${NEUTRAL.accent}`);
  });
});
