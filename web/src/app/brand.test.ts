/// <reference types="node" />
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { isWorkspace } from '../api/guards';
import type { Workspace } from '../api/types';
import { BRAND_PRESETS, CANVAS, INK, MUTED, NEUTRAL, activeBrand, contrast, pageTitle } from './brand';

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
    for (const t of [...Object.values(BRAND_PRESETS), NEUTRAL]) {
      expect(contrast('#ffffff', t.accent)).toBeGreaterThanOrEqual(4.5);
      expect(contrast('#ffffff', t.accentHover)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(INK, t.soft)).toBeGreaterThanOrEqual(4.5);
      expect(contrast(t.accent, t.soft)).toBeGreaterThanOrEqual(4.5);
      // D16: active nav item and the primary KPI card (white text, hint at 86% opacity → test plain white).
      expect(contrast(t.navInk, t.navCurrent)).toBeGreaterThanOrEqual(4.5);
      expect(contrast('#ffffff', t.kpi)).toBeGreaterThanOrEqual(7);
    }
    for (const bg of ['#ffffff', CANVAS]) {
      expect(contrast(INK, bg)).toBeGreaterThanOrEqual(7);
      expect(contrast(MUTED, bg)).toBeGreaterThanOrEqual(4.5);
      expect(contrast('#63726c', bg)).toBeGreaterThanOrEqual(4.5); // idle nav text
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
      expect(rule).toContain(`--nav-current: ${p.navCurrent}`);
      expect(rule).toContain(`--nav-ink: ${p.navInk}`);
      expect(rule).toContain(`--kpi: ${p.kpi}`);
    }
    expect(css).toContain(`--accent: ${NEUTRAL.accent}`);
    for (const v of [`--nav-current: ${NEUTRAL.navCurrent}`, `--nav-ink: ${NEUTRAL.navInk}`, `--kpi: ${NEUTRAL.kpi}`,
      `--ink: ${INK}`, `--muted: ${MUTED}`, `--canvas: ${CANVAS}`, '--nav-ink-idle: #63726c']) {
      expect(css).toContain(v);
    }
  });
});
