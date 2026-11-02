import { Component, Suspense, lazy, useCallback, useEffect, useLayoutEffect, useRef, useState, type ReactNode } from 'react';
import logoUrl from '../assets/logo-qash.svg';
import { ApiError, type Api } from '../api/client';
import type { Meta, Session, Workspace } from '../api/types';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { SelectField } from '../components/Field';
import { Icon } from '../components/Icon';
import { ErrorState, ForbiddenState, LoadingState, NotFoundState } from '../components/States';
import { PlannedPage } from '../pages/PlannedPage';
import { drafts } from './drafts';
import { navigate, resolvePendingNav, setNavGuard, useBoot, useHashRoute, usePendingNav, useScope, type ScopeData } from './hooks';
import { initials, roleLabel } from './labels';
import { allowedPages, leavesScope, pageDef, parseHash, toHash, type PageDef, type PageId } from './routes';
import { activeBrand, pageTitle, type ActiveBrand } from './brand';
import { freshest } from './workspaces';

// G14: one chunk per page, fetched on first visit (same origin, hashed file names). The shell,
// the API client and the planned-page placeholder stay in the entry chunk.
const DashboardPage = lazy(() => import('../pages/DashboardPage').then((m) => ({ default: m.DashboardPage })));
const TicketsPage = lazy(() => import('../pages/TicketsPage').then((m) => ({ default: m.TicketsPage })));
const SettingsPage = lazy(() => import('../pages/SettingsPage').then((m) => ({ default: m.SettingsPage })));
const ReportsPage = lazy(() => import('../pages/ReportsPage').then((m) => ({ default: m.ReportsPage })));
const IntegrationsPage = lazy(() => import('../pages/IntegrationsPage').then((m) => ({ default: m.IntegrationsPage })));

export function App({ api }: { api: Api }) {
  const { boot, retry } = useBoot(api);

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
    // Production waits for office SSO; there is deliberately no local login form.
    return (
      <main className="main">
        <img className="brand-logo" src={logoUrl} alt="Qash" />
        <div className="state">
          <h1>Masuk belum tersedia</h1>
          <p className="muted">Akses lingkungan ini memakai SSO kantor yang belum dikonfigurasi. Hubungi administrator.</p>
        </div>
      </main>
    );
  }
  return <Shell api={api} meta={boot.meta} session={boot.session} workspaces={boot.workspaces} onReenter={retry} />;
}

/** Logo of the active company's preset, or a neutral text mark when none is configured. */
function BrandMark({ brand, className }: { brand: ActiveBrand; className?: string }) {
  return (
    <span className={className} data-testid="brand-mark" data-brand-key={brand.key ?? 'neutral'}>
      {brand.preset
        ? <img className="brand-logo" data-brand-logo={brand.preset.key} src={brand.preset.logo} alt={brand.preset.logoAlt} />
        : <span className="brand-mark" aria-label="ITSM">ITSM</span>}
    </span>
  );
}

function BrandBlock({ brand }: { brand: ActiveBrand }) {
  return (
    <div className="brand">
      <BrandMark brand={brand} />
      {brand.key === 'ajr' && <span className="brand-stripe" aria-hidden="true" />}
      {brand.name && <span className="tiny muted brand-name">{brand.name}</span>}
    </div>
  );
}

function Shell({ api, meta, session, workspaces: bootWorkspaces, onReenter }: {
  api: Api;
  meta: Meta;
  session: Session;
  workspaces: Workspace[];
  onReenter: () => void;
}) {
  const route = useHashRoute();
  const [attempt, setAttempt] = useState(0);
  const scope = useScope(api, route.workspaceId, attempt);
  const [drawer, setDrawer] = useState(false);
  const [toast, setToast] = useState<string | null>(null);
  const [pendingSwitch, setPendingSwitch] = useState<Workspace | null>(null);
  // The workspace <select> that started a guarded switch: Cancel returns focus to it.
  const switchOrigin = useRef<HTMLElement | null>(null);
  const pendingNav = usePendingNav();
  // The switcher list; refreshed after a settings save so a rename or a new company shows
  // without a reload. A failed refresh keeps the previous list (the save itself succeeded).
  const [workspaces, setWorkspaces] = useState(bootWorkspaces);
  // Only the newest refresh may land: an older, slower answer must not bring back old names or branding.
  const listSeq = useRef(0);
  const refreshWorkspaces = useCallback(() => {
    const seq = ++listSeq.current;
    api.listWorkspaces().then((l) => { if (seq === listSeq.current) setWorkspaces(l); }, () => undefined);
  }, [api]);
  const first = workspaces[0];

  // No workspace in the URL: open the first one the server lists.
  useEffect(() => {
    if (!route.workspaceId && first) navigate(toHash({ workspaceId: first.id, page: route.page }), true);
  }, [route.workspaceId, route.page, first]);

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
  const pages = data ? allowedPages(data.permissions.permissions, session.uatSuperadmin) : [];
  const current = pageDef(route.page);
  const listed = workspaces.find((w) => w.id === route.workspaceId) ?? null;
  // Branding follows the workspace in the URL (no URL yet: the first one, which it is about to open).
  // It is derived in the same render as the route, so a switch never shows the previous company.
  const brand = activeBrand(route.workspaceId ? listed : first ?? null);
  useLayoutEffect(() => {
    const root = document.documentElement;
    if (brand.key) root.dataset.brand = brand.key;
    else delete root.dataset.brand;
    document.title = pageTitle(brand);
  }, [brand.key, brand.name]);

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

  function switchWorkspace(id: string, origin: HTMLElement) {
    const next = workspaces.find((w) => w.id === id);
    if (!next || id === route.workspaceId) return;
    // A dirty intake draft in the current workspace: keep it, discard it or stay (C13/O05).
    if (route.workspaceId && drafts.dirty(session.user.id, route.workspaceId)) {
      // The drawer stays open on mobile so Cancel can hand focus back to the same picker.
      switchOrigin.current = origin;
      setPendingSwitch(next);
      return;
    }
    go(next);
  }
  function go(next: Workspace) {
    setDrawer(false);
    setPendingSwitch(null);
    navigate(toHash({ workspaceId: next.id, page: route.page }), false, true);
    setToast(`Workspace aktif: ${next.code} · ${next.name}`);
  }

  // Answer to the leave guard: the mobile drawer closes like a normal nav pick.
  function leave(discard: boolean) {
    if (discard && route.workspaceId) drafts.discard(session.user.id, route.workspaceId);
    setDrawer(false);
    resolvePendingNav(true);
  }

  const switcher = (
    <SelectField label="Workspace" className="workspace" value={listed?.id ?? ''}
      hint={data ? roleLabel(data.permissions) : undefined}
      onChange={(e) => switchWorkspace(e.target.value, e.currentTarget)}>
      {!listed && <option value="">Pilih workspace</option>}
      {workspaces.map((w) => <option key={w.id} value={w.id}>{w.code} · {w.name}</option>)}
    </SelectField>
  );

  const nav = (onPick?: () => void) => (
    <nav className="nav" aria-label="Navigasi utama">
      {data && route.workspaceId
        ? pages.map((p) => (
          <a key={p.id} href={toHash({ workspaceId: route.workspaceId!, page: p.id })}
            aria-current={p.id === route.page ? 'page' : undefined} onClick={(e) => {
              if (e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return;
              e.preventDefault();
              if (navigate(e.currentTarget.getAttribute('href') ?? '')) onPick?.();
            }}>
            <Icon name={p.id} /> {p.label}
          </a>
        ))
        : <div className="nav-skeleton" role="status"><span className="sr-only">Memuat navigasi…</span>
          <div className="skeleton" aria-hidden="true" /><div className="skeleton" aria-hidden="true" /></div>}
    </nav>
  );

  return (
    <>
      <a className="skip-link" href="#main" onClick={(e) => { e.preventDefault(); document.getElementById('main')?.focus(); }}>
        Lewati ke konten
      </a>
      <div className="shell">
        <aside className="sidebar" aria-label="Sidebar">
          <BrandBlock brand={brand} />
          {switcher}
          {nav()}
          <Profile session={session} />
        </aside>
        <main className="main" id="main" tabIndex={-1}>
          <div className="top">
            <div className="top-left">
              <Button className="mobile-only" iconOnly aria-label="Buka navigasi"
                aria-haspopup="dialog" onClick={() => setDrawer(true)}><Icon name="menu" /></Button>
              <BrandMark brand={brand} className="top-brand" />
              <span className="breadcrumb">{listed ? `${listed.code} / ${current.label}` : current.label}</span>
            </div>
          </div>
          {meta.uat && (
            <div className="uat-banner" role="note" aria-label="Mode uji">
              <strong>{meta.uat.banner}</strong> · Data dummy · Tanpa SSO/koneksi produksi. Akses Superadmin bukan
              bukti izin CS/Lead CS.
            </div>
          )}
          <Content key={route.workspaceId ?? ''} api={api} userId={session.user.id} scope={scope} page={current} pages={pages}
            query={route.query} uat={session.uatSuperadmin} firstWorkspace={first ?? null} listed={listed}
            onWorkspacesChanged={refreshWorkspaces}
            onRetry={() => setAttempt((n) => n + 1)} onReenter={onReenter} onNotice={setToast} />
          <div className="footer">{brand.name ? `${brand.name} · ` : ''}ITSM · kandidat D15 (rilis develop) · data dummy UAT</div>
        </main>
      </div>
      <Dialog open={drawer} onClose={() => setDrawer(false)} title="Workspace & navigasi" variant="drawer">
        <BrandBlock brand={brand} />
        {switcher}
        {nav(() => setDrawer(false))}
        <Profile session={session} />
      </Dialog>
      <Dialog open={!!pendingSwitch} onClose={() => setPendingSwitch(null)} title="Draf tiket belum terkirim"
        initialFocus=".switch-cancel" returnFocus={switchOrigin}
        footer={pendingSwitch && (
          <>
            <Button className="switch-cancel" onClick={() => setPendingSwitch(null)}>Batal</Button>
            <Button onClick={() => {
              if (route.workspaceId) drafts.discard(session.user.id, route.workspaceId);
              go(pendingSwitch);
            }}>Buang draf &amp; pindah</Button>
            <Button variant="primary" onClick={() => go(pendingSwitch)}>Simpan draf &amp; pindah</Button>
          </>
        )}>
        <p>Ada draf tiket di workspace <strong>{listed ? `${listed.code} · ${listed.name}` : 'ini'}</strong> yang belum
          terkirim.</p>
        <p className="tiny muted">Simpan: draf tetap di memori tab ini untuk akun Anda dan workspace tersebut, dan muncul
          lagi saat Anda kembali. Draf tidak pernah dipindah ke workspace {pendingSwitch?.code ?? 'lain'} dan hilang saat
          halaman dimuat ulang.</p>
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
        <p>Ada draf tiket di workspace <strong>{listed ? `${listed.code} · ${listed.name}` : 'ini'}</strong> yang belum
          terkirim. Anda akan membuka <strong>{pendingNav !== null ? navTargetLabel(pendingNav, workspaces) : ''}</strong>.</p>
        <p className="tiny muted">Lanjut, simpan draf: draf tetap di memori tab ini dan muncul lagi saat Anda membuka
          Buat tiket di workspace ini. Draf tidak disimpan ke perangkat dan hilang saat halaman dimuat ulang.</p>
      </Dialog>
      {toast && <div className="toast" role="status">{toast}</div>}
    </>
  );
}

function navTargetLabel(hash: string, workspaces: Workspace[]): string {
  const to = parseHash(hash);
  const w = workspaces.find((x) => x.id === to.workspaceId);
  return w ? `${w.code} / ${pageDef(to.page).label}` : pageDef(to.page).label;
}

function Profile({ session }: { session: Session }) {
  return (
    <div className="profile">
      <span className="avatar" aria-hidden="true">{initials(session.user.displayName)}</span>
      <div>
        <div className="profile-name">{session.user.displayName}</div>
        <p className="tiny muted">{session.uatSuperadmin ? 'Superadmin UAT · data dummy' : 'Pengguna'}</p>
      </div>
    </div>
  );
}

function Content({ api, userId, scope, page, pages, query, uat, firstWorkspace, listed, onWorkspacesChanged, onRetry, onReenter, onNotice }: {
  api: Api;
  userId: string;
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
  if (scope.status === 'loading') return <LoadingState label="Memuat workspace…" />;
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
        <ErrorState title="Sesi berakhir" body="Sesi uji tidak lagi aktif. Masuk lagi ke Mode uji untuk melanjutkan."
          requestId={e.requestId} onRetry={onReenter} />
      );
    }
    if (e instanceof ApiError && e.status === 403) return <ForbiddenState />;
    return (
      <ErrorState title="Workspace belum dapat dimuat" body={errorText(e)}
        requestId={e instanceof ApiError ? e.requestId : null} onRetry={onRetry} />
    );
  }
  if (!pages.some((p) => p.id === page.id)) return <ForbiddenState />;
  const data = { ...scope.data, workspace: freshest(scope.data.workspace, listed) };
  return (
    <PageChunkBoundary key={page.id}>
      <Suspense fallback={<LoadingState label="Memuat halaman…" />}>
        <Page id={page.id} api={api} userId={userId} data={data} query={query} uat={uat} onNotice={onNotice}
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

function Page({ id, api, userId, data, query, uat, onNotice, onWorkspacesChanged }: {
  id: PageId; api: Api; userId: string; data: ScopeData; query: string; uat: boolean; onNotice: (text: string) => void;
  onWorkspacesChanged: () => void;
}) {
  switch (id) {
    case 'dashboard':
      return <DashboardPage api={api} workspace={data.workspace} uat={uat} query={query}
        canReports={data.permissions.permissions.includes('report.view')} />;
    case 'tickets':
      return <TicketsPage api={api} workspace={data.workspace} userId={userId} permissions={data.permissions} query={query} />;
    case 'settings':
      return <SettingsPage api={api} workspace={data.workspace} uat={uat} permissions={data.permissions.permissions} onNotice={onNotice}
        onWorkspacesChanged={onWorkspacesChanged} />;
    case 'reports':
      return <ReportsPage api={api} workspace={data.workspace} uat={uat} query={query} onNotice={onNotice} />;
    case 'integrations':
      return <IntegrationsPage api={api} workspace={data.workspace} onNotice={onNotice} />;
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
