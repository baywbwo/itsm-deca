import { useEffect, useRef, useState } from 'react';
import logoUrl from '../assets/logo-qash.svg';
import { ApiError, type Api } from '../api/client';
import type { Meta, Session, Workspace } from '../api/types';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { SelectField } from '../components/Field';
import { Icon } from '../components/Icon';
import { ErrorState, ForbiddenState, LoadingState, NotFoundState } from '../components/States';
import { ComponentsPage } from '../pages/ComponentsPage';
import { DashboardPage } from '../pages/DashboardPage';
import { PlannedPage } from '../pages/PlannedPage';
import { TicketsPage } from '../pages/TicketsPage';
import { navigate, useBoot, useHashRoute, useScope, type ScopeData } from './hooks';
import { initials, roleLabel } from './labels';
import { allowedPages, pageDef, toHash, type PageDef, type PageId } from './routes';

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

function Shell({ api, meta, session, workspaces, onReenter }: {
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

  function switchWorkspace(id: string) {
    const next = workspaces.find((w) => w.id === id);
    if (!next || id === route.workspaceId) return;
    setDrawer(false);
    navigate(toHash({ workspaceId: id, page: route.page }));
    setToast(`Workspace aktif: ${next.code} · ${next.name}`);
  }

  const switcher = (
    <SelectField label="Workspace" className="workspace" value={listed?.id ?? ''}
      hint={data ? roleLabel(data.permissions) : undefined}
      onChange={(e) => switchWorkspace(e.target.value)}>
      {!listed && <option value="">Pilih workspace</option>}
      {workspaces.map((w) => <option key={w.id} value={w.id}>{w.code} · {w.name}</option>)}
    </SelectField>
  );

  const nav = (onPick?: () => void) => (
    <nav className="nav" aria-label="Navigasi utama">
      {data && route.workspaceId
        ? pages.map((p) => (
          <a key={p.id} href={toHash({ workspaceId: route.workspaceId!, page: p.id })}
            aria-current={p.id === route.page ? 'page' : undefined} onClick={onPick}>
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
          <div className="brand">
            <img className="brand-logo" src={logoUrl} alt="Qash" />
            <span className="tiny muted">DECA Operations</span>
          </div>
          {switcher}
          {nav()}
          <Profile session={session} />
        </aside>
        <main className="main" id="main" tabIndex={-1}>
          <div className="top">
            <div className="top-left">
              <Button className="mobile-only" iconOnly aria-label="Buka navigasi"
                aria-haspopup="dialog" onClick={() => setDrawer(true)}><Icon name="menu" /></Button>
              <span className="breadcrumb">{listed ? `${listed.code} / ${current.label}` : current.label}</span>
            </div>
          </div>
          {meta.uat && (
            <div className="uat-banner" role="note" aria-label="Mode uji">
              <strong>{meta.uat.banner}</strong> · Data dummy · Tanpa SSO/koneksi produksi. Akses Superadmin bukan
              bukti izin CS/Lead CS.
            </div>
          )}
          <Content key={route.workspaceId ?? ''} api={api} scope={scope} page={current} pages={pages}
            query={route.query} uat={session.uatSuperadmin} firstWorkspace={first ?? null}
            onRetry={() => setAttempt((n) => n + 1)} onReenter={onReenter} />
          <div className="footer">ITSM Qash · kandidat D06 (List/Board tiket) · data dummy UAT</div>
        </main>
      </div>
      <Dialog open={drawer} onClose={() => setDrawer(false)} title="Workspace & navigasi" variant="drawer">
        {switcher}
        {nav(() => setDrawer(false))}
        <Profile session={session} />
      </Dialog>
      {toast && <div className="toast" role="status">{toast}</div>}
    </>
  );
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

function Content({ api, scope, page, pages, query, uat, firstWorkspace, onRetry, onReenter }: {
  api: Api;
  scope: ReturnType<typeof useScope>;
  page: PageDef;
  pages: PageDef[];
  query: string;
  uat: boolean;
  firstWorkspace: Workspace | null;
  onRetry: () => void;
  onReenter: () => void;
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
  return <Page id={page.id} api={api} data={scope.data} query={query} uat={uat} />;
}

function Page({ id, api, data, query, uat }: { id: PageId; api: Api; data: ScopeData; query: string; uat: boolean }) {
  switch (id) {
    case 'dashboard':
      return <DashboardPage api={api} workspace={data.workspace} uat={uat} query={query}
        canReports={data.permissions.permissions.includes('report.view')} />;
    case 'tickets':
      return <TicketsPage api={api} workspace={data.workspace} permissions={data.permissions} query={query} />;
    case 'components':
      return uat ? <ComponentsPage /> : <ForbiddenState />;
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
