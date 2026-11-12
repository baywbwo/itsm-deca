// One internal icon set (18px, 1.75 stroke) so the shell does not pull a new dependency.
// Decorative by default (aria-hidden); icon-only buttons must carry their own label.
const PATHS = {
  filter: 'M4 7h16M7 12h10M10 17h4',
  chevron: 'm8 10 4 4 4-4',
  arrow: 'M5 12h14m-5-5 5 5-5 5',
  user: 'M16 7a4 4 0 1 1-8 0 4 4 0 0 1 8 0zM4 21v-2a8 8 0 0 1 16 0v2',
  clock: 'M21 12a9 9 0 1 1-18 0 9 9 0 0 1 18 0zM12 7v5l3 2',
  check: 'm5 12 4 4L19 6',
  menu: 'M4 6h16M4 12h16M4 18h16',
  close: 'M6 6l12 12M18 6L6 18',
  dashboard: 'M4 4h7v7H4zM13 4h7v4h-7zM13 10h7v10h-7zM4 13h7v7H4z',
  tickets: 'M4 6h16M4 12h16M4 18h10',
  reports: 'M5 20V10M12 20V4M19 20v-7',
  settings: 'M12 15a3 3 0 1 0 0-6 3 3 0 0 0 0 6zM19 12l2-1-1-3-2 .2-1.3-1.3.2-2-3-1-1 2h-2l-1-2-3 1 .2 2L6.3 8.2 4.3 8l-1 3 2 1v0l-2 1 1 3 2-.2 1.3 1.3-.2 2 3 1 1-2h2l1 2 3-1-.2-2 1.3-1.3 2 .2 1-3z',
  integrations: 'M9 7V3M15 7V3M7 7h10v4a5 5 0 0 1-10 0zM12 16v5',
  components: 'M4 4h7v7H4zM13 4h7v7h-7zM4 13h7v7H4zM13 13h7v7h-7z',
  board: 'M4 5h4v14H4zM10 5h4v9h-4zM16 5h4v6h-4z',
  search: 'M11 18a7 7 0 1 0 0-14 7 7 0 0 0 0 14zM20 20l-4-4',
  bell: 'M6 16V11a6 6 0 0 1 12 0v5l2 2H4zM10 21h4',
  plus: 'M12 5v14M5 12h14',
  logout: 'M15 4h4v16h-4M10 8l-4 4 4 4M6 12h10',
  users: 'M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM2 21v-1a7 7 0 0 1 14 0v1M16 3.5a4 4 0 0 1 0 7M18 14a6 6 0 0 1 4 6v1',
} as const;
export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 18 }: { name: IconName; size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor"
      strokeWidth={1.75} strokeLinecap="round" strokeLinejoin="round" aria-hidden="true" focusable="false">
      <path d={PATHS[name]} />
    </svg>
  );
}
