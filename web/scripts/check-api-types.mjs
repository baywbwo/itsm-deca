#!/usr/bin/env node
// Fails when src/api/schema.d.ts is not exactly what openapi-typescript generates from the
// committed OpenAPI document (types are generated, never hand-edited).
import { execFileSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const tmp = mkdtempSync(join(tmpdir(), 'itsm-web-api-'));
try {
  const out = join(tmp, 'schema.d.ts');
  execFileSync('npx', ['openapi-typescript', '../openapi/openapi.json', '-o', out], { stdio: 'ignore' });
  const fresh = readFileSync(out, 'utf8');
  const committed = readFileSync('src/api/schema.d.ts', 'utf8');
  if (fresh !== committed) {
    console.error('check:api FAIL: src/api/schema.d.ts differs from openapi/openapi.json; run npm run gen:api');
    process.exit(1);
  }
  console.log('check:api PASS: schema.d.ts matches openapi/openapi.json');
} finally {
  rmSync(tmp, { recursive: true, force: true });
}
