import type { Workspace } from '../api/types';

/**
 * The newer of two server views of the same workspace row, by `version`. The scoped view
 * (loaded on switch) keeps the caller's role; a fresher list entry only updates the profile
 * fields a settings save can change, so a rename shows without a reload. Never widens access.
 */
export function freshest(scoped: Workspace, listed: Workspace | null | undefined): Workspace {
  if (!listed || listed.id !== scoped.id || listed.version <= scoped.version) return scoped;
  return { ...scoped, name: listed.name, timezone: listed.timezone, version: listed.version };
}
