import bmtLogo from '../assets/logo-bmt.svg';
import ajrLogo from '../assets/logo-ajr.png';
import decaLogo from '../assets/logo-qash.svg';
import type { BrandKey, Workspace } from '../api/types';

/**
 * Company branding presets bundled with the app (D15). The server stores only the preset key
 * (`deca` | `ajr` | `bmt` | null), never a URL or file, so a logo can only be one of these assets.
 * CP-UI1: the app wears one theme (owner prototype, THEME below) for every company; a preset
 * now names the company logo shown in Pengaturan, no longer a colour scheme.
 */
export interface BrandPreset {
  key: Exclude<BrandKey, null>;
  label: string;
  logo: string;
  logoAlt: string;
}

export const BRAND_PRESETS: Record<Exclude<BrandKey, null>, BrandPreset> = {
  deca: { key: 'deca', label: 'DOB / DECA (Qash)', logo: decaLogo, logoAlt: 'Qash' },
  bmt: { key: 'bmt', label: 'BMT', logo: bmtLogo, logoAlt: 'BMT' },
  ajr: { key: 'ajr', label: 'AJR', logo: ajrLogo, logoAlt: 'AJR' },
};

/**
 * Owner prototype tokens (design/reference-owner-v2, reference-owner-v1 README). app.css `:root`
 * carries the same values; brand.test.ts keeps them in step and checks the contrast pairs.
 */
export const THEME = {
  sapphire: '#003571',
  sapphire800: '#002a5a',
  sapphire100: '#e7eef8',
  green: '#58ea60',
  eco: '#04ca02',
  canvas: '#f4f7f9',
  ink: '#0f1b2d',
  ink2: '#31435a',
  muted: '#56687c',
  /** Idle sidebar text on sapphire. */
  navIdle: '#c9d7e8',
  /** Sidebar group captions on sapphire (large caps, decorative-level text). */
  navCaption: '#93aac9',
  line: '#e3eaf1',
} as const;

export interface ActiveBrand {
  key: BrandKey;
  name: string;
  preset: BrandPreset | null;
}

/**
 * The brand of the workspace in the URL, read from the switcher list already in memory: it
 * changes in the same render as the route, so switching never shows the previous company's
 * name, and no late request can bring it back.
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
