import { describe, expect, it } from 'vitest';
import { credentialRetargeted } from './integrations';

const jira = { baseUrl: 'https://kantor.atlassian.net', email: 'svc@example.test', appId: '' };
const lark = { baseUrl: '', email: '', appId: 'cli_a1' };

describe('credentialRetargeted (CP5B-01)', () => {
  it('flags a new Jira address or account, not a cosmetic edit', () => {
    expect(credentialRetargeted('jira', jira, { ...jira, baseUrl: 'https://evil.example' })).toBe(true);
    expect(credentialRetargeted('jira', jira, { ...jira, email: 'lain@example.test' })).toBe(true);
    expect(credentialRetargeted('jira', jira, { ...jira, baseUrl: ' https://Kantor.atlassian.net/ ' })).toBe(false);
    expect(credentialRetargeted('jira', jira, { ...jira, appId: 'x' })).toBe(false);
  });

  it('treats an empty Lark address as the international default', () => {
    expect(credentialRetargeted('lark', lark, { ...lark, baseUrl: 'https://open.larksuite.com' })).toBe(false);
    expect(credentialRetargeted('lark', lark, { ...lark, baseUrl: 'https://open.feishu.cn' })).toBe(true);
    expect(credentialRetargeted('lark', lark, { ...lark, appId: 'cli_b2' })).toBe(true);
  });
});
