import ajrLogo from '../assets/logo-ajr.png';
import decaLogo from '../assets/logo-qash.svg';
import type { BrandKey, Workspace } from '../api/types';

/**
 * Company branding presets bundled with the app (D15). The server stores only the preset key
 * (`deca` | `ajr` | null), never a URL or file, so a logo can only be one of these assets. The
 * colours live in app.css under `[data-brand=…]`; the values here are kept in step for the
 * contrast test.
 */
export interface BrandPreset {
  key: Exclude<BrandKey, null>;
  label: string;
  logo: string;
  logoAlt: string;
  /** White text on `accent` (primary buttons) and `ink` on `soft` must stay ≥ 4.5:1. */
  accent: string;
  accentHover: string;
  soft: string;
  /** D16: active nav item (`navInk` on `navCurrent`) and the primary KPI card (white on `kpi`). */
  navCurrent: string;
  navInk: string;
  kpi: string;
}

export const BRAND_PRESETS: Record<Exclude<BrandKey, null>, BrandPreset> = {
  deca: {
    key: 'deca', label: 'DECA (logo Qash, hijau)', logo: decaLogo, logoAlt: 'Qash',
    accent: '#245c31', accentHover: '#16482f', soft: '#e8f1e5',
    navCurrent: '#eafbe5', navInk: '#174b2b', kpi: '#003571',
  },
  ajr: {
    key: 'ajr', label: 'AJR (ungu)', logo: ajrLogo, logoAlt: 'AJR',
    accent: '#3b299c', accentHover: '#2c1f78', soft: '#efecf9',
    navCurrent: '#efecf9', navInk: '#2c1f78', kpi: '#2c1f78',
  },
};

/** No preset configured (or no workspace yet): a neutral slate theme with a text mark, no logo. */
export const NEUTRAL = {
  accent: '#3f4a56', accentHover: '#2e3640', soft: '#eef0f3', navCurrent: '#eef0f3', navInk: '#152b3a', kpi: '#2e3640',
} as const;

/** Body text and secondary text on the white cards and the canvas (app.css `--ink`, `--muted`). */
export const INK = '#152b3a';
export const MUTED = '#5f6d74';
export const CANVAS = '#f7f9f7';

export interface ActiveBrand {
  key: BrandKey;
  name: string;
  preset: BrandPreset | null;
}

/**
 * The brand of the workspace in the URL, read from the switcher list already in memory: it
 * changes in the same render as the route, so switching never shows the previous company's
 * logo, and no late request can bring it back.
 */
export function activeBrand(listed: Workspace | null): ActiveBrand {
  if (!listed) return { key: null, name: '', preset: null };
  const preset = listed.brandKey ? BRAND_PRESETS[listed.brandKey] ?? null : null;
  return { key: preset ? preset.key : null, name: listed.brandName, preset };
}

export const pageTitle = (b: ActiveBrand) => (b.name ? `${b.name} · ITSM` : 'ITSM');

/** WCAG relative-luminance contrast ratio of two #rrggbb colours. */
export function contrast(a: string, b: string): number {
  const lin = (hex: string, i: number) => {
    const v = parseInt(hex.slice(i, i + 2), 16) / 255;
    return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4;
  };
  const lum = (hex: string) => 0.2126 * lin(hex, 1) + 0.7152 * lin(hex, 3) + 0.0722 * lin(hex, 5);
  const [x, y] = [lum(a), lum(b)];
  return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
}
