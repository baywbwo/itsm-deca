#!/usr/bin/env node
// Turns the review build into one self-contained HTML file that opens from file:// (no server).
import { readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';

const dir = process.argv[2] ?? 'dist-review';
let html = readFileSync(join(dir, 'review.html'), 'utf8');
const read = (href) => readFileSync(join(dir, href.replace(/^\.\//, '')), 'utf8');

html = html.replace(/<script type="module" crossorigin src="([^"]+)"><\/script>/g, (_, src) =>
  `<script type="module">${read(src).replaceAll('</script', '<\\/script')}</script>`);
html = html.replace(/<link rel="stylesheet" crossorigin href="([^"]+)">/g, (_, href) =>
  `<style>${read(href).replaceAll('</style', '<\\/style')}</style>`);
if (/(src|href)="\.\/assets\//.test(html)) throw new Error('unresolved asset reference left in review HTML');

const out = join(dir, 'itsm-d06-review.html');
writeFileSync(out, html);
console.log(`${out} (${(html.length / 1024).toFixed(0)} KiB)`);
