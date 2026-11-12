import { Component, Suspense, lazy, useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import { ApiError, type Api } from '../api/client';
import type { Meta, Session, Workspace } from '../api/types';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { Icon, type IconName } from '../components/Icon';
import { ErrorState, ForbiddenState, LoadingState, NotFoundState } from '../components/States';
import { Popover } from '../components/Popover';
import { PlannedPage } from '../pages/PlannedPage';
import { AccountMenu, ChangePasswordScreen, LoginScreen, accountRole, type LoginNotice } from '../pages/AccountScreens';
import { MfaEnrolScreen, MfaReloginScreen, StepUpDialog } from '../pages/MfaScreens';
import logoWhiteUrl from '../assets/qash-logo-white.png';
import { LOGGED_OUT_ALL_TEXT, SESSION_ENDED_TEXT, SESSION_ENDED_TITLE } from './account';
import { drafts } from './drafts';
import { navigate, resolvePendingNav, setNavGuard, useBoot, useHashRoute, usePendingNav, useScope, type ScopeData } from './hooks';
import { initials, roleLabel } from './labels';
import { allowedPages, crumbLabel, leavesScope, navGroups, pageDef, parseHash, toHash, type NavItem, NEW_TICKET_FLAG, type PageDef, type PageId } from './routes';
import { activeBrand, pageTitle } from './brand';
import { freshest } from './workspaces';

// G14: one chunk per page, fetched on first visit (same origin, hashed file names). The shell,
// the API client and the planned-page placeholder stay in the entry chunk.
const DashboardPage = lazy(() => import('../pages/DashboardPage').then((m) => ({ default: m.DashboardPage })));
const TicketsPage = lazy(() => import('../pages/TicketsPage').then((m) => ({ default: m.TicketsPage })));
const SettingsPage = lazy(() => import('../pages/SettingsPage').then((m) => ({ default: m.SettingsPage })));
const ReportsPage = lazy(() => import('../pages/ReportsPage').then((m) => ({ default: m.ReportsPage })));
const IntegrationsPage = lazy(() => import('../pages/IntegrationsPage').then((m) => ({ default: m.IntegrationsPage })));
const UsersPage = lazy(() => import('../pages/UsersPage').then((m) => ({ default: m.UsersPage })));

export function App({ api }: { api: Api }) {
  const { boot, retry } = useBoot(api);
  // Why the login is showing again (session ended, logged out everywhere); cleared on login.
  const [notice, setNotice] = useState<LoginNotice | null>(null);
  // Username typed at login in this tab, for the client-side "not the username" check only.
  const [username, setUsername] = useState<string | null>(null);

  // CP3a: a 401 in the middle of work (idle/absolute limit, revoked elsewhere) reloads the
  // session, which lands on the login with the reason. In-memory drafts stay for the same user.
  useEffect(() => api.onSessionEnded(() => {
    setNotice({ title: SESSION_ENDED_TITLE, text: SESSION_ENDED_TEXT });
    retry();
  }), [api, retry]);

  const loggedIn = useCallback((name: string) => { setNotice(null); setUsername(name); retry(); }, [retry]);
  const loggedOutAll = useCallback(() => {
    drafts.clear();
    setNotice({ title: 'Keluar dari semua perangkat', text: LOGGED_OUT_ALL_TEXT });
    retry();
  }, [retry]);

  if (boot.status === 'loading') {
    return <main className="main"><LoadingState label="Menyiapkan sesi…" /></main>;
  }
  if (boot.status === 'error') {
    return (
      <main className="main">
        <ErrorState title="Aplikasi belum dapat dimuat" body={errorText(boot.error)}
          requestId={boot.error instanceof ApiError ? boot.error.requestId : null} onRetry={retry} />
      </main>
    );
  }
  if (boot.status === 'no_access') {
    return <LoginScreen api={api} meta={boot.meta} notice={notice} onSuccess={loggedIn} />;
  }
  if (boot.status === 'mfa_relogin') {
    return <MfaReloginScreen onLogout={() => { void api.logout().finally(retry); }} />;
  }
  if (boot.status === 'mfa_setup') {
    return <MfaEnrolScreen api={api} session={boot.session} onDone={retry}
      onLogout={() => { void api.logout().finally(retry); }} />;
  }
  if (boot.status === 'must_change') {
    return <ChangePasswordScreen api={api} session={boot.session} username={username} onChanged={retry}
      onLogout={() => { void api.logout().finally(retry); }} />;
  }
  return <Shell api={api} meta={boot.meta} session={boot.session} workspaces={boot.workspaces} onReenter={retry}
    username={username} onLoggedOutAll={loggedOutAll} />;
}

/** CP-UI1: the product mark is the same for every company (owner prototype); the company shows in the breadcrumb. */
function SideBrand() {
  return (
    <div className="side-brand">
      <img className="side-logo" src={logoWhiteUrl} alt="Qash" />
      <span className="side-sub"><b>SERVICE DESK</b><small>ITSM</small></span>
    </div>
  );
}

const NAV_ICON: Record<NavItem['key'], IconName> = {
  dashboard: 'dashboard', tickets: 'tickets', board: 'board', new: 'plus', reports: 'reports',
  settings: 'settings', integrations: 'integrations', users: 'users',
};

function Shell({ api, meta, session, workspaces: bootWorkspaces, onReenter, username, onLoggedOutAll }: {
  api: Api;
  meta: Meta;
  session: Session;
  workspaces: Workspace[];
  username: string | null;
  onReenter: () => void;
  onLoggedOutAll: () => void;
}) {
  const route = useHashRoute();
  const [attempt, setAttempt] = useState(0);
  const scope = useScope(api, route.workspaceId, attempt);
  const [drawer, setDrawer] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const pendingNav = usePendingNav();
  // CP5a: no workspace switcher. The server lists only the caller's company (UAT Superadmin:
  // every company, for the UAT company admin). Refreshed after a UAT company save so a rename
  // shows without a reload; a failed refresh keeps the previous list (the save itself succeeded).
  const [workspaces, setWorkspaces] = useState(bootWorkspaces);
  // Only the newest refresh may land: an older, slower answer must not bring back old names or branding.
  const listSeq = useRef(0);
  const refreshWorkspaces = useCallback(() => {
    const seq = ++listSeq.current;
    api.listWorkspaces().then((l) => { if (seq === listSeq.current) setWorkspaces(l); }, () => undefined);
  }, [api]);
  const first = workspaces[0];

  // The company is decided by the server: no workspace in the URL, or one the caller cannot open
  // (old bookmark, another company), opens the caller's own company instead.
  const routeListed = workspaces.some((w) => w.id === route.workspaceId);
  useEffect(() => {
    if (first && !routeListed) navigate(toHash({ workspaceId: first.id, page: route.page }), true);
  }, [routeListed, route.page, first]);

  useEffect(() => {
    if (!toast) return;
    const t = window.setTimeout(() => setToast(null), 3000);
    return () => window.clearTimeout(t);
  }, [toast]);

  // Move focus to the page title after navigation so screen readers announce the new page.
  const lastRoute = useRef<string | null>(null);
  const routeKey = `${route.workspaceId ?? ''}/${route.page}`;
  const ready = scope.status === 'success';
  useEffect(() => {
    if (!ready || lastRoute.current === routeKey) return;
    const initial = lastRoute.current === null;
    lastRoute.current = routeKey;
    if (!initial) document.getElementById('page-title')?.focus();
  }, [ready, routeKey]);

  const data = scope.status === 'success' ? scope.data : null;
  const pages = data ? allowedPages(data.permissions.permissions, session.uatSuperadmin, session.platformAdmin) : [];
  const current = pageDef(route.page);
  const listed = workspaces.find((w) => w.id === route.workspaceId) ?? null;
  // Branding follows the workspace in the URL (no URL yet: the first one, which it is about to open).
  // It is derived in the same render as the route, so a switch never shows the previous company.
  const brand = activeBrand(route.workspaceId ? listed : first ?? null);
  useLayoutEffect(() => { document.title = pageTitle(brand); }, [brand.key, brand.name]);
  const groups = data ? navGroups(pages, route, data.permissions.permissions.includes('ticket.create')) : [];
  const canSearch = !!route.workspaceId && pages.some((p) => p.id === 'tickets');
  const logout = () => {
    if (drafts.anyDirty() && !window.confirm('Keluar akan membuang draf yang belum disimpan. Lanjutkan?')) return;
    void api.logout().then(() => { drafts.clear(); onReenter(); });
  };
  // Header search = the Tiket list `q` filter. On Tiket it keeps the other filters, sort and view.
  const search = (q: string) => {
    const params = new URLSearchParams(route.page === 'tickets' ? route.query : '');
    params.delete(NEW_TICKET_FLAG);
    params.set('q', q);
    setDrawer(false);
    navigate(toHash({ workspaceId: route.workspaceId!, page: 'tickets', query: params.toString() }));
  };

  // In-memory intake drafts must not vanish silently: warn before a reload drops them.
  useEffect(() => {
    const onUnload = (e: BeforeUnloadEvent) => {
      if (!drafts.anyDirty()) return;
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', onUnload);
    return () => window.removeEventListener('beforeunload', onUnload);
  }, []);

  // Leaving the page or workspace (nav link, Back/Forward, typed URL) with a dirty intake draft
  // asks first; the draft stays in memory unless the user discards it. Filters/sort never ask.
  useEffect(() => setNavGuard((from, to) =>
    leavesScope(from, to) && !!from.workspaceId && drafts.dirty(session.user.id, from.workspaceId)), [session.user.id]);

  // Answer to the leave guard: the mobile drawer closes like a normal nav pick.
  function leave(discard: boolean) {
    if (discard && route.workspaceId) drafts.discard(session.user.id, route.workspaceId);
    setDrawer(false);
    resolvePendingNav(true);
  }

  const nav = (onPick?: () => void) => (
    <nav className="nav" aria-label="Navigasi utama">
      {data && route.workspaceId
        ? groups.map((g) => (
          <div key={g.label} className="nav-group" role="group" aria-labelledby={`nav-${g.label}${onPick ? '-d' : ''}`}>
            <div className="nav-caption" id={`nav-${g.label}${onPick ? '-d' : ''}`}>{g.label}</div>
            {g.items.map((it) => (
              <a key={it.key} className="nav-item" href={it.href} aria-current={it.current ? 'page' : undefined} onClick={(e) => {
                if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
                e.preventDefault();
                if (navigate(e.currentTarget.getAttribute('href') ?? '')) onPick?.();
              }}>
                <Icon name={NAV_ICON[it.key]} /><span>{it.label}</span>
              </a>
            ))}
          </div>
        ))
        : <div className="nav-skeleton" role="status"><span className="sr-only">Memuat navigasi…</span>
          <div className="skeleton" aria-hidden="true" /><div className="skeleton" aria-hidden="true" /></div>}
    </nav>
  );
  const role = data ? roleLabel(data.permissions) : accountRole(session);

  return (
    <>
      <a className="skip-link" href="#main" onClick={(e) => { e.preventDefault(); document.getElementById('main')?.focus(); }}>
        Lewati ke konten
      </a>
      <div className="shell">
        <aside className="sidebar" aria-label="Sidebar">
          <SideBrand />
          {nav()}
          <div className="side-foot"><Profile session={session} role={role} /></div>
        </aside>
        <main className="main" id="main" tabIndex={-1}>
          <header className="top">
            <div className="top-left">
              <button type="button" className="iconbtn mobile-only" aria-label="Buka navigasi"
                aria-haspopup="dialog" onClick={() => setDrawer(true)}><Icon name="menu" /></button>
              <CompanyCrumb company={listed} page={crumbLabel(route)} />
              <span className="top-title" aria-hidden="true">{crumbLabel(route)}</span>
            </div>
            <div className="top-right">
              {canSearch && <TicketSearch onSearch={search} shortcut />}
              <Popover className="icon-menu" label="Notifikasi" title="Notifikasi"
                trigger={<><Icon name="bell" /><span className="sr-only">Notifikasi</span></>}>
                {() => (
                  <div className="notif-empty" tabIndex={-1}>
                    <strong>Belum ada notifikasi</strong>
                    <p className="tiny muted">Pemberitahuan tiket akan tampil di sini setelah tersedia.</p>
                  </div>
                )}
              </Popover>
              <AccountMenu api={api} session={session} role={role} passwordMode={meta.accessMode === 'password'}
                username={username} onLoggedOutAll={onLoggedOutAll} onLogout={logout} onNotice={setToast} />
              <StepUpDialog api={api} />
            </div>
          </header>
          <div className="content">
          <Content key={route.workspaceId ?? ''} api={api} userId={session.user.id} mfaEnabled={session.mfaEnabled} scope={scope} page={current} pages={pages}
            query={route.query} uat={session.uatSuperadmin} firstWorkspace={first ?? null} listed={listed}
            onWorkspacesChanged={refreshWorkspaces}
            onRetry={() => setAttempt((n) => n + 1)} onReenter={onReenter} onNotice={setToast} />
          <div className="footer">{brand.name ? `${brand.name} · ` : ''}Qash Service Desk</div>
          </div>
        </main>
      </div>
      <Dialog open={drawer} onClose={() => setDrawer(false)} title="Navigasi" variant="drawer">
        <SideBrand />
        <DrawerCompany company={listed} />
        {canSearch && <TicketSearch onSearch={search} />}
        {nav(() => setDrawer(false))}
        <div className="side-foot"><Profile session={session} role={role} /></div>
      </Dialog>
      <Dialog open={pendingNav !== null} onClose={() => resolvePendingNav(false)} title="Tinggalkan draf tiket?"
        initialFocus=".nav-stay"
        footer={pendingNav !== null && (
          <>
            <Button className="nav-stay" onClick={() => resolvePendingNav(false)}>Tetap di sini</Button>
            <Button onClick={() => leave(true)}>Buang draf &amp; lanjut</Button>
            <Button variant="primary" onClick={() => leave(false)}>Lanjut, simpan draf</Button>
          </>
        )}>
        <p>Ada draf tiket di <strong>{listed ? `${listed.code} · ${listed.name}` : 'ini'}</strong> yang belum
          terkirim. Anda akan membuka <strong>{pendingNav !== null ? navTargetLabel(pendingNav, workspaces) : ''}</strong>.</p>
        <p className="tiny muted">Lanjut, simpan draf: draf tetap di memori tab ini dan muncul lagi saat Anda membuka
          Buat tiket. Draf tidak disimpan ke perangkat dan hilang saat halaman dimuat ulang.</p>
      </Dialog>
      {toast && <div className="toast" role="status">{toast}</div>}
    </>
  );
}

/** CP5a: the company comes from the account (SSO per company), so the header names it as plain
 * text: no dropdown, no switching. */
export function CompanyCrumb({ company, page }: { company: Workspace | null; page: string }) {
  return (
    <div className="crumb">
      {company && <><span className="crumb-ws" data-testid="crumb-company">{companyName(company)}</span>
        <span className="crumb-sep" aria-hidden="true">/</span></>}
      <b>{page}</b>
    </div>
  );
}

/** Mobile drawer counterpart of {@link CompanyCrumb}: a label, not a field. */
export function DrawerCompany({ company }: { company: Workspace | null }) {
  if (!company) return null;
  return <p className="drawer-field drawer-company"><span>Perusahaan</span><b>{companyName(company)}</b></p>;
}

const companyName = (w: Workspace) => w.brandName || w.name;

function navTargetLabel(hash: string, workspaces: Workspace[]): string {
  const to = parseHash(hash);
  const w = workspaces.find((x) => x.id === to.workspaceId);
  return w ? `${w.code} / ${pageDef(to.page).label}` : pageDef(to.page).label;
}

/** Header search over the Tiket list (the existing `q` filter); no new query or API. "/" focuses it. */
function TicketSearch({ onSearch, shortcut = false }: { onSearch: (q: string) => void; shortcut?: boolean }) {
  const input = useRef<HTMLInputElement>(null);
  useEffect(() => {
    if (!shortcut) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey) return;
      const t = e.target as HTMLElement | null;
      if (t && (t.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(t.tagName))) return;
      if (document.querySelector('dialog[open]') || !input.current?.offsetParent) return;
      e.preventDefault();
      input.current.focus();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [shortcut]);
  return (
    <form className="top-search" role="search" aria-label="Pencarian tiket" onSubmit={(e) => {
      e.preventDefault();
      const el = input.current;
      const q = el?.value.trim();
      if (!el || !q) return;
      onSearch(q);
      el.value = '';
    }}>
      <Icon name="search" size={16} />
      <input ref={input} name="q" type="search" aria-label="Cari tiket" placeholder="Cari nomor atau judul tiket…" maxLength={100}
        aria-keyshortcuts={shortcut ? '/' : undefined} />
      {shortcut && <kbd aria-hidden="true">/</kbd>}
    </form>
  );
}

function Profile({ session, role }: { session: Session; role: string }) {
  return (
    <div className="profile">
      <span className="avatar" aria-hidden="true">{initials(session.user.displayName)}</span>
      <div>
        <div className="profile-name">{session.user.displayName}</div>
        <p className="profile-role">{role}</p>
      </div>
    </div>
  );
}

function Content({ api, userId, mfaEnabled, scope, page, pages, query, uat, firstWorkspace, listed, onWorkspacesChanged, onRetry, onReenter, onNotice }: {
  api: Api;
  userId: string;
  mfaEnabled: boolean;
  scope: ReturnType<typeof useScope>;
  page: PageDef;
  pages: PageDef[];
  query: string;
  uat: boolean;
  firstWorkspace: Workspace | null;
  listed: Workspace | null;
  onWorkspacesChanged: () => void;
  onRetry: () => void;
  onReenter: () => void;
  onNotice: (text: string) => void;
}) {
  if (scope.status === 'idle') {
    return <NotFoundState />;
  }
  if (scope.status === 'loading') return <LoadingState label="Memuat data perusahaan…" />;
  if (scope.status === 'error') {
    const e = scope.error;
    if (e instanceof ApiError && e.status === 404) {
      return (
        <NotFoundState action={firstWorkspace && (
          <Button variant="primary" onClick={() => navigate(toHash({ workspaceId: firstWorkspace.id, page: 'dashboard' }))}>
            Buka {firstWorkspace.code}
          </Button>
        )} />
      );
    }
    if (e instanceof ApiError && e.status === 401) {
      return (
        <ErrorState title="Sesi berakhir" body="Sesi Anda tidak lagi aktif. Masuk kembali untuk melanjutkan."
          requestId={e.requestId} onRetry={onReenter} />
      );
    }
    if (e instanceof ApiError && e.status === 403) return <ForbiddenState />;
    return (
      <ErrorState title="Data perusahaan belum dapat dimuat" body={errorText(e)}
        requestId={e instanceof ApiError ? e.requestId : null} onRetry={onRetry} />
    );
  }
  if (!pages.some((p) => p.id === page.id)) return <ForbiddenState />;
  const data = { ...scope.data, workspace: freshest(scope.data.workspace, listed) };
  return (
    <PageChunkBoundary key={page.id}>
      <Suspense fallback={<LoadingState label="Memuat halaman…" />}>
        <Page id={page.id} api={api} userId={userId} mfaEnabled={mfaEnabled} data={data} query={query} uat={uat} onNotice={onNotice}
          onWorkspacesChanged={onWorkspacesChanged} />
      </Suspense>
    </PageChunkBoundary>
  );
}

/**
 * A page chunk that cannot be fetched (network drop, or a redeploy replaced the hashed files)
 * shows a reload instead of a blank shell. React.lazy keeps a failed import, so only a reload
 * fetches it again; the draft guard still asks before unsaved input is lost.
 */
class PageChunkBoundary extends Component<{ children: ReactNode }, { failed: boolean }> {
  override state = { failed: false };
  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }
  override render() {
    if (!this.state.failed) return this.props.children;
    return (
      <ErrorState title="Halaman belum dapat dimuat" body="Bagian aplikasi untuk halaman ini gagal diambil dari server. Muat ulang halaman."
        retryLabel="Muat ulang" onRetry={() => location.reload()} />
    );
  }
}

function Page({ id, api, userId, mfaEnabled, data, query, uat, onNotice, onWorkspacesChanged }: {
  id: PageId; api: Api; userId: string; mfaEnabled: boolean; data: ScopeData; query: string; uat: boolean; onNotice: (text: string) => void;
  onWorkspacesChanged: () => void;
}) {
  switch (id) {
    case 'dashboard':
      return <DashboardPage api={api} workspace={data.workspace} query={query}
        canReports={data.permissions.permissions.includes('report.view')}
        canCreate={data.permissions.permissions.includes('ticket.create')} />;
    case 'tickets':
      return <TicketsPage api={api} workspace={data.workspace} userId={userId} permissions={data.permissions} query={query} />;
    case 'settings':
      return <SettingsPage api={api} workspace={data.workspace} uat={uat} permissions={data.permissions.permissions} onNotice={onNotice}
        onWorkspacesChanged={onWorkspacesChanged} />;
    case 'reports':
      return <ReportsPage api={api} workspace={data.workspace} query={query} onNotice={onNotice} />;
    case 'integrations':
      return <IntegrationsPage api={api} workspace={data.workspace} onNotice={onNotice} />;
    case 'users':
      return <UsersPage api={api} userId={userId} mfaEnabled={mfaEnabled} onNotice={onNotice} />;
    default:
      return <PlannedPage page={pageDef(id)} workspace={data.workspace} query={query} />;
  }
}

function errorText(e: unknown): string {
  if (e instanceof ApiError) {
    if (e.code === 'network') return 'Server tidak dapat dihubungi. Periksa koneksi lalu coba lagi.';
    if (e.code === 'uat_not_available') return 'Mode uji tidak tersedia di lingkungan ini.';
    return e.message;
  }
  return 'Terjadi kesalahan yang tidak terduga.';
}
