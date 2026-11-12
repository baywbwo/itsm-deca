/// <reference types="node" />
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { isWorkspace } from '../api/guards';
import type { Workspace } from '../api/types';
import { BRAND_PRESETS, THEME, activeBrand, contrast, pageTitle } from './brand';

const ws: Workspace = {
  id: 'a1e8dd4a-a301-42f1-ac2a-58db5c57811b', code: 'DUMA', name: 'Workspace A (dummy)', timezone: 'Asia/Jakarta',
  companyId: 'c1', version: 3, myRole: 'cs', brandKey: 'deca', brandName: 'PT Dummy Alfa (uji)',
};
const css = readFileSync(new URL('../styles/app.css', import.meta.url), 'utf8').toLowerCase();
const overrides = readFileSync(new URL('../styles/workspace.css', import.meta.url), 'utf8').toLowerCase();

describe('D15 company branding', () => {
  it('resolves the preset of the listed workspace, neutral without one', () => {
    expect(activeBrand(ws)).toMatchObject({ key: 'deca', name: 'PT Dummy Alfa (uji)', preset: { logoAlt: 'Qash' } });
    expect(activeBrand({ ...ws, brandKey: 'ajr' }).preset?.logoAlt).toBe('AJR');
    expect(activeBrand({ ...ws, brandKey: 'bmt' }).preset?.logoAlt).toBe('BMT');
    expect(activeBrand({ ...ws, brandKey: 'bmt' }).key).toBe('bmt');
    expect(activeBrand({ ...ws, brandKey: null })).toMatchObject({ key: null, preset: null, name: 'PT Dummy Alfa (uji)' });
    expect(activeBrand(null)).toEqual({ key: null, name: '', preset: null });
    expect(pageTitle(activeBrand(ws))).toBe('PT Dummy Alfa (uji) · ITSM');
    expect(pageTitle(activeBrand(null))).toBe('ITSM');
  });

  it('accepts only the bundled preset keys from the server', () => {
    expect(isWorkspace(ws)).toBe(true);
    expect(isWorkspace({ ...ws, brandKey: null })).toBe(true);
    expect(isWorkspace({ ...ws, brandKey: 'bmt' })).toBe(true);
    for (const bad of ['https://example.com/logo.png', '../logo', 'DECA', 'qash', '', undefined]) {
      expect(isWorkspace({ ...ws, brandKey: bad })).toBe(false);
    }
    expect(isWorkspace({ ...ws, brandName: null })).toBe(false);
    expect(Object.keys(BRAND_PRESETS).sort()).toEqual(['ajr', 'bmt', 'deca']);
  });
});

describe('CP-UI1 owner prototype theme', () => {
  it('keeps text, buttons, nav and chips readable (WCAG AA 4.5:1)', () => {
    const t = THEME;
    expect(contrast('#ffffff', t.sapphire)).toBeGreaterThanOrEqual(7); // primary button, sidebar
    expect(contrast('#ffffff', t.sapphire800)).toBeGreaterThanOrEqual(7); // primary hover
    expect(contrast(t.sapphire, t.green)).toBeGreaterThanOrEqual(4.5); // active nav item
    expect(contrast(t.navIdle, t.sapphire)).toBeGreaterThanOrEqual(4.5); // idle nav item
    expect(contrast(t.navCaption, t.sapphire)).toBeGreaterThanOrEqual(4.5); // group captions
    for (const bg of ['#ffffff', t.canvas]) {
      expect(contrast(t.ink, bg)).toBeGreaterThanOrEqual(7);
      expect(contrast(t.ink2, bg)).toBeGreaterThanOrEqual(7);
      expect(contrast(t.muted, bg)).toBeGreaterThanOrEqual(4.5);
    }
    // Secondary text also sits on the sapphire-100 brief panel and the board columns (axe CP-UI1).
    for (const bg of [t.sapphire100, '#eef2f6']) expect(contrast(t.muted, bg)).toBeGreaterThanOrEqual(4.5);
    // Status chips: [text, background] as in app.css.
    for (const [fg, bg] of [['#1b4ea8', '#e8f1fe'], [t.sapphire, t.sapphire100], ['#8a5b00', '#fef3d7'],
      ['#5b21b6', '#efeafe'], ['#12693a', '#e6f7e9'], ['#516176', '#eef2f6'], ['#a22b2b', '#fdecec']] as const) {
      expect(contrast(fg, bg)).toBeGreaterThanOrEqual(4.5);
    }
  });

  it('the stylesheet carries the theme tokens, one theme for every company', () => {
    for (const v of [`--sapphire: ${THEME.sapphire}`, `--sapphire-800: ${THEME.sapphire800}`, `--sapphire-100: ${THEME.sapphire100}`,
      `--green: ${THEME.green}`, `--eco: ${THEME.eco}`, `--canvas: ${THEME.canvas}`, `--ink: ${THEME.ink}`,
      `--ink-2: ${THEME.ink2}`, `--muted: ${THEME.muted}`, `--nav-idle: ${THEME.navIdle}`, `--nav-caption: ${THEME.navCaption}`,
      `--line: ${THEME.line}`, '--accent: var(--sapphire)', '--radius-control: 9px', '--radius-card: 14px']) {
      expect(css).toContain(v);
    }
    // No per-company colour blocks, and the old operational blue is no longer the primary.
    expect(css + overrides).not.toContain('[data-brand');
    expect(css + overrides).not.toMatch(/--action-blue|#075bc7|#0057c8/);
    expect(overrides).not.toMatch(/--(?:accent|sapphire|canvas|ink|muted)\s*:/);
  });
});
