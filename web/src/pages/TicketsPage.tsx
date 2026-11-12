import { useEffect, useId, useRef, useState, type DragEvent } from 'react';
import { ApiError, SESSION_RELOAD_TITLE, isSessionReloadNeeded, rateLimitText, sessionReloadText, isAbort, type Api } from '../api/client';
import type {
  CatalogItem,
  TicketAllowedAction,
  TicketBoard,
  TicketList,
  TicketListBasis,
  TicketListItem,
  TicketStatus,
  TicketStatusCode,
  Workspace,
  WorkspacePermissions,
} from '../api/types';
import { ageLabel, instantLabel, localDate, nextDay, num, tzLabel, zonedMidnight } from '../app/format';
import { navigate, useTicketData, useTicketOptions, type TicketOptions } from '../app/hooks';
import {
  activeTicketFilters,
  emptyTicketFilters,
  parseDetailQuery,
  parseTicketQuery,
  ticketApiQuery,
  ticketQuery,
  toHash,
  canonicalColumns,
  COLUMN_LABELS,
  DEFAULT_TICKET_COLUMNS,
  isDefaultColumns,
  TICKET_COLUMNS,
  type TicketColumn,
  type TicketFilterKey,
  type TicketParams,
  type TicketSort,
  type TicketView,
  withDetail,
  NEW_TICKET_FLAG,
} from '../app/routes';
import { Button } from '../components/Button';
import { Dialog } from '../components/Dialog';
import { PageHeader } from '../components/PageHeader';
import { Popover } from '../components/Popover';
import { EmptyState, ErrorState, ForbiddenState, NotFoundState } from '../components/States';
import { Badge, statusTone } from '../components/Badge';
import { Icon } from '../components/Icon';
import { initials } from '../app/labels';
import { planDrop } from '../app/boardDnd';
import {
  activePreset, allStatusCount, applyPreset, pageRange, PRESET_LABELS, presetsFor, priorityDot, shortInstant,
  urgentPriorityId, type TicketPreset,
} from '../app/ticketView';
import { AssignDialog } from './AssignDialog';
import { IntakeDialog } from './IntakeDialog';
import { SavedViews } from './SavedViews';
import { TicketDetailView } from './TicketDetail';

/** Lifecycle order of the D01 freeze; labels come from the server when it has sent them. */
const STATUS_ORDER: readonly TicketStatusCode[] = [
  'new', 'open', 'pending_customer', 'pending_engineering', 'resolved', 'closed',
];
const STATUS_FALLBACK: Record<TicketStatusCode, string> = {
  new: 'Baru',
  open: 'Ditangani',
  pending_customer: 'Menunggu pelanggan',
  pending_engineering: 'Menunggu pekerjaan eksternal',
  resolved: 'Terselesaikan',
  closed: 'Ditutup (arsip)',
};

const SORT_LABELS: Record<TicketSort, string> = {
  updated_at: 'Terakhir diperbarui',
  received_at: 'Waktu diterima',
  number: 'Nomor tiket',
};

/** CP-UI2b: short labels for the compact sort control (its accessible name says "Urutkan"). */
const SORT_SHORT: Record<TicketSort, string> = { updated_at: 'Diperbarui', received_at: 'Diterima', number: 'Nomor' };

const SEARCH_DEBOUNCE_MS = 300;

type DimKey = 'owner' | 'channel' | 'category' | 'priority';
const DIMENSIONS: readonly { key: DimKey; label: string; unset: { value: string; label: string } }[] = [
  { key: 'owner', label: 'PIC', unset: { value: 'unassigned', label: 'Belum ditugaskan' } },
  { key: 'channel', label: 'Kanal', unset: { value: 'none', label: 'Tanpa kanal' } },
  { key: 'category', label: 'Kategori', unset: { value: 'none', label: 'Tanpa kategori' } },
  { key: 'priority', label: 'Prioritas', unset: { value: 'none', label: 'Tanpa prioritas' } },
];

/** CP-UI2: shown as selects in the toolbar row (prototype order); the rest stay in the Filter popover. */
const INLINE_DIMS: readonly DimKey[] = ['priority', 'category', 'owner'];
const INLINE_ALL: Record<DimKey, string> = { priority: 'Semua prioritas', category: 'Semua kategori', owner: 'Semua PIC', channel: 'Semua kanal' };

function dimOptions(o: TicketOptions | null, k: DimKey): { id: string; label: string; active: boolean }[] {
  if (!o) return [];
  if (k === 'owner') return o.owners.map((m) => ({ id: m.userId, label: m.displayName, active: m.active }));
  const list: CatalogItem[] = k === 'channel' ? o.channels : k === 'category' ? o.categories : o.priorities;
  return list.map((c) => ({ id: c.id, label: c.label, active: c.active }));
}

/**
 * O02/O03: one ticket scope, two views. Every filter, the sort and the view live in the URL
 * (`#/w/<ws>/tickets?view=board&status=…`), use the List API's own parameter names (so a
 * Dashboard drilldown opens the same scope) and survive a view switch, reload and back. Counts
 * are the server's whole-scope numbers, never the loaded rows.
 */
export function TicketsPage({ api, workspace, userId, permissions, query = '' }: {
  api: Api;
  workspace: Workspace;
  userId: string;
  permissions: WorkspacePermissions;
  query?: string;
}) {
  const params = parseTicketQuery(query);
  const apiQuery = ticketApiQuery(params);
  const [attempt, setAttempt] = useState(0);
  const state = useTicketData(api, workspace.id, params.view, apiQuery, attempt);
  const options = useTicketOptions(api, workspace.id);
  const [labels, setLabels] = useState<Partial<Record<TicketStatusCode, string>>>({});
  const [transition, setTransition] = useState<{ ticket: TicketListItem; to: TicketStatusCode | null } | null>(null);
  const dp = parseDetailQuery(query);
  const [detailRefresh, setDetailRefresh] = useState(0);
  const [assign, setAssign] = useState<TicketListItem | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const noticeRef = useRef<HTMLDivElement>(null);
  // After a save the list remounts and the trigger is gone: land on the result instead.
  useEffect(() => { if (notice) noticeRef.current?.focus(); }, [notice]);
  const searchRef = useRef<HTMLInputElement>(null);
  const tz = workspace.timezone;
  const data = state.status === 'success' ? state.data : null;
  const canCreate = permissions.permissions.includes('ticket.create');
  // CP-UI3: "Tiket baru" is its own page (06) at tickets?new=1, so Back and reload keep it.
  // Without ticket.create the flag is only dropped (replace).
  const wantsNew = new URLSearchParams(query).get(NEW_TICKET_FLAG) === '1';
  useEffect(() => {
    if (wantsNew && !canCreate) navigate(toHash({ workspaceId: workspace.id, page: 'tickets', query: ticketQuery(params) }), true);
    // eslint-disable-next-line react-hooks/exhaustive-deps -- one shot per flag
  }, [wantsNew, canCreate]);
  const openCreate = () => {
    const q = new URLSearchParams(ticketQuery(params));
    q.set(NEW_TICKET_FLAG, '1');
    navigate(toHash({ workspaceId: workspace.id, page: 'tickets', query: q.toString() }));
  };
  const canAssign = permissions.permissions.includes('ticket.assign_any') || permissions.permissions.includes('ticket.assign_self');

  // Server labels seen so far (status select keeps working while the next answer loads).
  useEffect(() => {
    if (!data) return;
    const seen: TicketStatus[] = data.view === 'list'
      ? [...data.list.countsByStatus.map((c) => c.status), ...data.list.items.map((t) => t.status)]
      : data.board.columns.map((c) => c.status);
    setLabels((old) => {
      const next = { ...old };
      for (const s of seen) next[s.code] = s.label;
      return next;
    });
  }, [data]);
  const statusLabel = (c: TicketStatusCode) => labels[c] ?? STATUS_FALLBACK[c];

  const go = (next: TicketParams, replace = false) =>
    navigate(toHash({ workspaceId: workspace.id, page: 'tickets', query: ticketQuery(next) }), replace);
  const setFilter = (k: TicketFilterKey, v: string | null, replace = false) =>
    go({ ...params, filters: { ...params.filters, [k]: v || null } }, replace);
  const active = activeTicketFilters(params);
  const reset = () => {
    searchRef.current?.focus();
    go({ ...params, filters: emptyTicketFilters() });
  };
  const reload = () => setAttempt((n) => n + 1);
  // D08 deep link: the open ticket lives in the URL next to the list scope (reload/back keep it).
  const detailHash = (id: string | null, full = false) =>
    toHash({ workspaceId: workspace.id, page: 'tickets', query: withDetail(ticketQuery(params), id, full) });
  const openDetail = (t: TicketListItem) => navigate(detailHash(t.id));
  const closeDetail = () => {
    const id = dp.ticketId;
    navigate(detailHash(null));
    // After full page or a deep link the opener is gone: land on the same ticket's row/card.
    window.setTimeout(() => {
      const row = id ? document.querySelector<HTMLElement>(`[data-open-ticket="${CSS.escape(id)}"]`) : null;
      if (row && (document.activeElement === document.body || !document.activeElement?.isConnected)) row.focus();
    }, 0);
  };

  function onDone(t: TicketListItem, message: string) {
    setTransition(null);
    setAssign(null);
    if (dp.ticketId === t.id) setDetailRefresh((n) => n + 1);
    setNotice(message);
    reload();
  }

  const dialogs = (
    <>
      {transition && (
        <TransitionDialog api={api} workspace={workspace} ticket={transition.ticket} initialTo={transition.to}
          onClose={() => setTransition(null)} onDone={onDone} onReloadList={reload} />
      )}
      {assign && (
        <AssignDialog api={api} workspace={workspace} userId={userId} permissions={permissions} ticket={assign}
          onClose={() => setAssign(null)} onDone={onDone} onReloadList={reload} />
      )}
    </>
  );
  if (dp.ticketId && dp.full) {
    return (
      <>
        <TicketDetailView key={`${workspace.id}|${dp.ticketId}`} api={api} workspace={workspace} ticketId={dp.ticketId}
          full refresh={detailRefresh} canAssign={canAssign} onClose={closeDetail}
          onToggleFull={() => navigate(detailHash(dp.ticketId, false), true)} onChanged={() => undefined}
          onTransition={(t) => setTransition({ ticket: t, to: null })} onAssign={setAssign} />
        {dialogs}
      </>
    );
  }

  if (wantsNew && canCreate) {
    return (
      <>
        <PageHeader title="Tiket baru" description="Formulir internal CS — kanal dipilih eksplisit, kontak pelapor dicatat pada kronologi"
          action={<Button onClick={() => go(params)}>Batal</Button>} />
        <IntakeDialog asPage api={api} workspace={workspace} userId={userId} permissions={permissions}
          onClose={() => go(params)}
          onCreated={(r) => {
            setNotice(`${r.ticket.displayNumber} ${r.replayed ? 'sudah tersimpan sebelumnya (kiriman ulang dikenali)' : 'tersimpan'}. Daftar dimuat ulang dari server.`);
            reload();
          }} />
      </>
    );
  }

  const total = data ? (data.view === 'list' ? data.list.total : data.board.total) : null;
  const urgentId = options ? urgentPriorityId(options.priorities) : null;
  const boardMode = params.view === 'board';
  const newButton = canCreate ? <Button variant="primary" onClick={openCreate}><Icon name="plus" size={16} /> Tiket baru</Button> : null;
  const toolbar = (
      <Toolbar api={api} workspaceId={workspace.id} params={params} options={options} tz={tz} statusLabel={statusLabel}
        searchRef={searchRef} onFilter={setFilter}
        onClear={(keys) => go({ ...params, filters: { ...params.filters, ...Object.fromEntries(keys.map((k) => [k, null])) } })}
        onSort={(sort, order) => go({ ...params, sort, order })}
        onApply={(p) => go(p)} activeCount={active.length} onReset={reset} />
  );
  return (
    <>
      {boardMode ? (
        <PageHeader title="Board" description="Pandangan per status · kartu dapat diseret antar kolom"
          action={(
            <div className="ph-acts">
              <Button onClick={() => go({ ...params, view: 'list' })}><Icon name="tickets" size={16} /> Tampilan daftar</Button>
              {newButton}
            </div>
          )} />
      ) : (
        <PageHeader title="Tiket"
          description={total === null
            ? `Daftar pengaduan ${workspace.name}`
            : `${num(total)} tiket · diurutkan berdasarkan ${SORT_LABELS[params.sort].toLowerCase()} ${params.order === 'asc' ? '(naik)' : '(turun)'}`}
          action={newButton ?? undefined} />
      )}
      <section className={boardMode ? 'board-page' : 'pcard list-card'} aria-label={boardMode ? 'Board tiket' : 'Daftar dan filter tiket'}>
      {boardMode ? (
        <details className="dashboard-filter-panel board-filter-panel" open={active.length > 0 || undefined}>
          <summary><span className="filter-heading"><Icon name="filter" size={16} /> Filter & urutan</span>
            <span className="filter-summary">{active.length ? `${active.length} filter aktif` : 'Semua tiket'}<Icon name="chevron" size={14} /></span></summary>
          {toolbar}
        </details>
      ) : toolbar}
      <p className="sr-only" role="status" aria-live="polite">
        {notice ?? (data ? `${num(data.view === 'list' ? data.list.total : data.board.total)} tiket dimuat.` : '')}
      </p>
      {notice && (
        <div className="notice row between" role="note" tabIndex={-1} ref={noticeRef}>
          <span>{notice}</span>
          <Button variant="ghost" onClick={() => setNotice(null)}>Tutup</Button>
        </div>
      )}
      <div id="ticket-panel">
        {state.status === 'loading' && <TicketSkeleton view={params.view} />}
        {state.status === 'error' && (
          <TicketError error={state.error} onRetry={() => { searchRef.current?.focus(); reload(); }}
            onReset={active.length ? reset : null} />
        )}
        {data?.view === 'list' && (
          <ListView key={state.status === "success" ? state.key : ""} api={api} workspace={workspace} list={data.list} params={params}
            apiQuery={apiQuery} filtered={active.length > 0} canCreate={canCreate}
            presets={presetsFor(urgentId)} preset={activePreset(params.filters, userId, urgentId)}
            onPreset={(p) => go({ ...params, filters: applyPreset(params.filters, p, userId, urgentId) })}
            onSort={(sort) => go({ ...params, sort, order: params.sort === sort && params.order === 'desc' ? 'asc' : 'desc' })}
            onStatus={(code) => setFilter('status', code && params.filters.status !== code ? code : null)}
            onOpen={openDetail} onTransition={(t) => setTransition({ ticket: t, to: null })}
            onReset={reset} onCreate={openCreate} />
        )}
        {data?.view === 'board' && (
          <BoardView key={state.status === "success" ? state.key : ""} api={api} workspace={workspace} board={data.board} apiQuery={apiQuery}
            filtered={active.length > 0} canCreate={canCreate}
            onOpen={openDetail} onTransition={(t, to) => setTransition({ ticket: t, to })}
            onRefused={setNotice} onReset={reset} onCreate={openCreate} />
        )}
      </div>
      </section>
      {transition && (
        <TransitionDialog api={api} workspace={workspace} ticket={transition.ticket} initialTo={transition.to}
          onClose={() => setTransition(null)} onDone={onDone} onReloadList={reload} />
      )}
      {dp.ticketId && (
        <TicketDetailView key={`${workspace.id}|${dp.ticketId}`} api={api} workspace={workspace} ticketId={dp.ticketId}
          full={dp.full} refresh={detailRefresh} canAssign={canAssign} onClose={closeDetail}
          onToggleFull={() => navigate(detailHash(dp.ticketId, !dp.full), true)}
          onChanged={() => { if (!dp.full) reload(); }}
          onTransition={(t) => setTransition({ ticket: t, to: null })} onAssign={setAssign} />
      )}
      {assign && (
        <AssignDialog api={api} workspace={workspace} userId={userId} permissions={permissions} ticket={assign}
          onClose={() => setAssign(null)} onDone={onDone} onReloadList={reload} />
      )}
    </>
  );
}

/** Filters inside the Filter popover (everything except search and status). */
const POPOVER_FILTERS: readonly TicketFilterKey[] = [
  'channel', 'receivedFrom', 'receivedBefore', 'resolvedFrom', 'resolvedBefore',
];

/**
 * C05/CP-UI2b one-row toolbar like the prototype (wraps on narrow screens): search (300 ms debounce, Enter applies now), status, Filter popover
 * (PIC/kanal/kategori/prioritas/tanggal, with an active-count badge and its own reset), sort, C32
 * saved view and one reset for everything. Active popover filters stay visible as removable chips.
 */
function Toolbar({ api, workspaceId, params, options, tz, statusLabel, searchRef, onFilter, onClear, onSort, onApply, activeCount, onReset }: {
  api: Api;
  workspaceId: string;
  params: TicketParams;
  options: TicketOptions | null;
  tz: string;
  statusLabel: (c: TicketStatusCode) => string;
  searchRef: React.RefObject<HTMLInputElement | null>;
  onFilter: (k: TicketFilterKey, v: string | null, replace?: boolean) => void;
  onClear: (keys: readonly TicketFilterKey[]) => void;
  onSort: (sort: TicketSort, order: 'asc' | 'desc') => void;
  onApply: (p: TicketParams) => void;
  activeCount: number;
  onReset: () => void;
}) {
  const applied = params.filters.q ?? '';
  const [text, setText] = useState(applied);
  const typed = useRef(false);
  const filterRef = useRef(onFilter);
  filterRef.current = onFilter;
  // Follow the URL (back/forward, reset, drilldown, saved view) unless the user is mid-typing.
  useEffect(() => {
    if (!typed.current) setText(applied);
  }, [applied]);
  useEffect(() => {
    if (!typed.current) return;
    const t = window.setTimeout(() => {
      typed.current = false;
      if (text.trim() !== applied) filterRef.current('q', text.trim() || null, true);
    }, SEARCH_DEBOUNCE_MS);
    return () => window.clearTimeout(t);
  }, [text, applied]);

  const status = params.filters.status;
  const statusKnown = !status || STATUS_ORDER.includes(status as TicketStatusCode);
  const statusSet = status && !statusKnown ? status.split(',') : [];
  const recvFrom = params.filters.receivedFrom ? localDate(params.filters.receivedFrom, tz) : null;
  const recvTo = params.filters.receivedBefore ? localDate(params.filters.receivedBefore, tz, true) : null;
  const resolved = params.filters.resolvedFrom || params.filters.resolvedBefore;
  const popoverActive = POPOVER_FILTERS.filter((k) => params.filters[k]);
  // Range bounds count as one filter each (received, resolved).
  const popoverCount = DIMENSIONS.filter((d) => params.filters[d.key] && !INLINE_DIMS.includes(d.key)).length +
    (recvFrom || recvTo ? 1 : 0) + (resolved ? 1 : 0);

  const dimLabel = (k: DimKey, value: string) => {
    const d = DIMENSIONS.find((x) => x.key === k)!;
    if (value === d.unset.value) return d.unset.label;
    const list = dimOptions(options, k);
    return value.split(',').map((id) => list.find((o) => o.id === id)?.label ?? (options ? 'tidak dikenal' : 'memuat…')).join(' + ');
  };
  const list = params.view === 'list';
  const chips: { key: string; text: string; clear: readonly TicketFilterKey[] }[] = [
    // List shows status as chips with counts; a combined status (dashboard drilldown) gets its own chip.
    ...(list && statusSet.length ? [{
      key: 'status',
      text: `Status: ${statusSet.map((c) => STATUS_ORDER.includes(c as TicketStatusCode) ? statusLabel(c as TicketStatusCode) : c).join(' + ')}`,
      clear: ['status'] as const,
    }] : []),
    ...DIMENSIONS.filter((d) => params.filters[d.key] && !INLINE_DIMS.includes(d.key)).map((d) => ({
      key: d.key, text: `${d.label}: ${dimLabel(d.key, params.filters[d.key]!)}`, clear: [d.key] as const,
    })),
    ...(recvFrom || recvTo ? [{
      key: 'received',
      text: `Diterima ${recvFrom ? `dari ${recvFrom}` : ''}${recvFrom && recvTo ? ' ' : ''}${recvTo ? `s.d. ${recvTo}` : ''}`,
      clear: ['receivedFrom', 'receivedBefore'] as const,
    }] : []),
    ...(resolved ? [{
      key: 'resolved',
      text: `Diselesaikan${params.filters.resolvedFrom ? ` sejak ${instantLabel(params.filters.resolvedFrom, tz)}` : ''}${params.filters.resolvedBefore ? ` sebelum ${instantLabel(params.filters.resolvedBefore, tz)}` : ''}`,
      clear: ['resolvedFrom', 'resolvedBefore'] as const,
    }] : []),
  ];

  return (
    <div className="toolbar filterbar ticket-toolbar" role="search" aria-label="Cari dan filter tiket">
      <div className="toolbar-row">
        <label className="toolbar-search">
          <span className="sr-only">Cari tiket</span>
          <input ref={searchRef} className="field-control" type="search" value={text} maxLength={100}
            placeholder="Cari nomor tiket atau subjek…"
            onChange={(e) => { typed.current = true; setText(e.target.value); }}
            onKeyDown={(e) => {
              if (e.key !== 'Enter') return;
              typed.current = false;
              onFilter('q', text.trim() || null);
            }} />
        </label>
        {INLINE_DIMS.map((k) => {
          const d = DIMENSIONS.find((x) => x.key === k)!;
          const value = params.filters[k] ?? '';
          const opts = dimOptions(options, k);
          const known = value === '' || value === d.unset.value || opts.some((o) => o.id === value);
          return (
            <label className="toolbar-select" key={k}>
              <span className="sr-only">{d.label}</span>
              <select className="field-control" value={value} onChange={(e) => onFilter(k, e.target.value || null)}>
                <option value="">{INLINE_ALL[k]}</option>
                {opts.map((o) => <option key={o.id} value={o.id}>{o.label}{o.active ? '' : ' (nonaktif)'}</option>)}
                <option value={d.unset.value}>{d.unset.label}</option>
                {!known && <option value={value}>{dimLabel(k, value)}</option>}
              </select>
            </label>
          );
        })}
        {!list && <label className="toolbar-select">
          <span className="sr-only">Status</span>
          <select className="field-control" value={status ?? ''} onChange={(e) => onFilter('status', e.target.value || null)}>
            <option value="">Semua status</option>
            {STATUS_ORDER.map((c) => <option key={c} value={c}>{statusLabel(c)}</option>)}
            {!statusKnown && status && (
              <option value={status}>{statusSet.map((c) => STATUS_ORDER.includes(c as TicketStatusCode)
                ? statusLabel(c as TicketStatusCode) : c).join(' + ')}</option>
            )}
          </select>
        </label>}
        <div className="toolbar-tools">
        <Popover className="filter-popover" label="Filter lanjutan" title="Filter"
          trigger={<>
            <span aria-hidden="true">⚲</span> Filter
            {popoverCount > 0 && <span className="count-badge" aria-label={`${popoverCount} aktif`}>{popoverCount}</span>}
          </>}>
          {(close) => (
            <>
              <div className="filter-grid">
                {DIMENSIONS.filter((d) => !INLINE_DIMS.includes(d.key)).map((d) => {
                  const value = params.filters[d.key] ?? '';
                  const list = dimOptions(options, d.key);
                  const known = value === '' || value === d.unset.value || list.some((o) => o.id === value);
                  return (
                    <label className="period" key={d.key}>
                      {d.label}
                      <select className="field-control" value={value} onChange={(e) => onFilter(d.key, e.target.value || null)}>
                        <option value="">Semua</option>
                        {list.map((o) => <option key={o.id} value={o.id}>{o.label}{o.active ? '' : ' (nonaktif)'}</option>)}
                        <option value={d.unset.value}>{d.unset.label}</option>
                        {!known && <option value={value}>{dimLabel(d.key, value)}</option>}
                      </select>
                    </label>
                  );
                })}
                <label className="period">
                  Diterima dari
                  <input className="field-control" type="date" value={recvFrom ?? ''}
                    onChange={(e) => onFilter('receivedFrom', e.target.value ? zonedMidnight(e.target.value, tz) : null)} />
                </label>
                <label className="period">
                  Diterima s.d.
                  <input className="field-control" type="date" value={recvTo ?? ''}
                    onChange={(e) => onFilter('receivedBefore', e.target.value ? zonedMidnight(nextDay(e.target.value), tz) : null)} />
                </label>
              </div>
              <p className="muted tiny">Diterapkan langsung ke List dan Board · waktu {tzLabel(tz)}.
                {resolved ? ' Rentang penyelesaian dari Dashboard tampil sebagai chip.' : ''}</p>
              <div className="popover-foot row between">
                <Button variant="ghost" disabled={popoverActive.length === 0} onClick={() => onClear(popoverActive)}>
                  Reset filter ini
                </Button>
                <Button variant="primary" onClick={close}>Selesai</Button>
              </div>
            </>
          )}
        </Popover>
        <label className="toolbar-select toolbar-sort">
          <span className="sr-only">Urutkan</span>
          <select className="field-control" value={`${params.sort}:${params.order}`}
            onChange={(e) => {
              const [s, o] = e.target.value.split(':');
              onSort(s as TicketSort, o === 'asc' ? 'asc' : 'desc');
            }}>
            {(Object.keys(SORT_LABELS) as TicketSort[]).flatMap((s) => [
              <option key={`${s}:desc`} value={`${s}:desc`}>{SORT_SHORT[s]} ↓</option>,
              <option key={`${s}:asc`} value={`${s}:asc`}>{SORT_SHORT[s]} ↑</option>,
            ])}
          </select>
        </label>
        {list && <ColumnChooser params={params} onApply={onApply} />}
        <SavedViews api={api} workspaceId={workspaceId} params={params} onApply={onApply} />
        {activeCount > 0 && (
          <Button variant="ghost" onClick={onReset} aria-label={`Reset semua filter (${activeCount} aktif)`}>
            Reset ({activeCount})
          </Button>
        )}
        </div>
      </div>
      {chips.length > 0 && (
        <ul className="chips" aria-label="Filter aktif">
          {chips.map((c) => (
            <li key={c.key} className="chip">
              {c.text}
              <Button variant="ghost" className="chip-x" aria-label={`Hapus filter ${c.text}`}
                onClick={() => onClear(c.clear)}>✕</Button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

/** Basis of every number on the page (contract `basis`), shown in words. */
function BasisNote({ basis, total, resolutionEvents, loaded }: {
  basis: TicketListBasis;
  total: number;
  resolutionEvents: number | null;
  loaded?: number;
}) {
  return (
    <p className="muted tiny basis">
      <strong>{num(total)} tiket</strong>
      {loaded !== undefined && ` · dimuat ${num(loaded)} dari ${num(total)}`}
      {' · '}dihitung server untuk seluruh filter (tiket unik). Filter PIC/kanal/kategori/prioritas memakai atribut tiket
      saat ini, bukan nilai saat peristiwa terjadi (lingkup operasional, bukan laporan historis).
      {basis.resolved === 'resolve_event_in_range' && resolutionEvents !== null && (
        <> Rentang penyelesaian: {num(total)} tiket unik dari {num(resolutionEvents)} peristiwa penyelesaian; tiket yang
          diselesaikan lebih dari sekali dalam rentang tampil satu kali.</>
      )}
    </p>
  );
}

function ListView({ api, workspace, list, params, apiQuery, filtered, canCreate, presets, preset, onPreset, onSort, onStatus, onOpen, onTransition, onReset, onCreate }: {
  api: Api;
  workspace: Workspace;
  list: TicketList;
  params: TicketParams;
  apiQuery: string;
  filtered: boolean;
  canCreate: boolean;
  presets: readonly TicketPreset[];
  preset: TicketPreset | null;
  onPreset: (p: TicketPreset) => void;
  onSort: (s: TicketSort) => void;
  onStatus: (code: TicketStatusCode | null) => void;
  onOpen: (t: TicketListItem) => void;
  onTransition: (t: TicketListItem) => void;
  onReset: () => void;
  onCreate: () => void;
}) {
  // CP-UI2 pager: pages already fetched stay in memory, so "Sebelumnya" never refetches; "Berikutnya"
  // follows the server cursor (bound to the same filters and sort). A reload starts at page 1.
  const [pages, setPages] = useState<{ items: TicketListItem[]; next: string | null }[]>(
    () => [{ items: list.items, next: list.nextCursor }]);
  const [index, setIndex] = useState(0);
  const [more, setMore] = useState<{ status: 'idle' | 'loading' } | { status: 'error'; error: unknown }>({ status: 'idle' });
  const ctl = useRef<AbortController | null>(null);
  const tableRef = useRef<HTMLDivElement>(null);
  useEffect(() => () => ctl.current?.abort(), []);
  const tz = workspace.timezone;
  const page = pages[index]!;
  const items = page.items;
  const range = pageRange(pages.map((p) => p.items.length), index);

  const show_ = (i: number) => {
    setIndex(i);
    tableRef.current?.focus();
  };
  async function next() {
    if (pages[index + 1]) return show_(index + 1);
    if (!page.next) return;
    ctl.current?.abort();
    const c = new AbortController();
    ctl.current = c;
    setMore({ status: 'loading' });
    try {
      const res = await api.listTickets(workspace.id, apiQuery, page.next, c.signal);
      setPages((old) => [...old.slice(0, index + 1), { items: res.items, next: res.nextCursor }]);
      setMore({ status: 'idle' });
      show_(index + 1);
    } catch (error) {
      if (!isAbort(error)) setMore({ status: 'error', error });
    }
  }

  const sortHeader = (key: TicketSort, label: string) => {
    const on = params.sort === key;
    return {
      ariaSort: on ? (params.order === 'asc' ? 'ascending' as const : 'descending' as const) : undefined,
      node: (
        <button type="button" className="sort" onClick={() => onSort(key)}>
          {label}<span aria-hidden="true">{on ? (params.order === 'asc' ? ' ↑' : ' ↓') : ' ↕'}</span>
        </button>
      ),
    };
  };
  const num_ = sortHeader('number', 'Nomor');
  const recv = sortHeader('received_at', 'Umur');
  const recvAt = sortHeader('received_at', 'Diterima');
  const upd = sortHeader('updated_at', 'Diperbarui');
  const status = params.filters.status;
  const show = (c: TicketColumn) => params.columns.includes(c);

  return (
    <>
      <div className="chip-rows">
        <div className="pchips" role="group" aria-label="Status (jumlah tanpa filter status; filter lain tetap berlaku)">
          <button type="button" className="pchip" aria-pressed={!status} onClick={() => onStatus(null)}>
            Semua <span className="n">{num(allStatusCount(list.countsByStatus))}</span>
          </button>
          {list.countsByStatus.map((c) => (
            <button key={c.status.code} type="button" className="pchip" aria-pressed={status === c.status.code}
              onClick={() => onStatus(c.status.code)}>
              {c.status.label} <span className="n">{num(c.count)}</span>
            </button>
          ))}
        </div>
        <div className="pchips presets" role="group" aria-label="Tampilan cepat">
          {presets.map((p) => (
            <button key={p} type="button" className="pchip" aria-pressed={preset === p} onClick={() => onPreset(p)}>
              {PRESET_LABELS[p]}
            </button>
          ))}
        </div>
      </div>
      {list.total === 0 ? (
        <div className="bd">
          {filtered
            ? <EmptyState title="Tidak ada tiket sesuai filter" body="Ubah atau reset filter untuk melihat tiket lain."
              action={<Button onClick={onReset}>Reset filter</Button>} />
            : <EmptyState title="Belum ada tiket" body={`Belum ada tiket di ${workspace.code}.`}
              action={canCreate ? <Button variant="primary" onClick={onCreate}>＋ Buat tiket</Button> : undefined} />}
        </div>
      ) : (
        <div className="ptable" role="region" aria-label="Daftar tiket" tabIndex={0} ref={tableRef}>
          <table className="tickets">
            <caption className="sr-only">Tiket {workspace.code} · urut {SORT_LABELS[params.sort].toLowerCase()} {params.order === 'asc' ? 'naik' : 'turun'} lalu ID · baris {num(range.from)}–{num(range.to)}</caption>
            <thead>
              <tr>
                <th scope="col" aria-sort={num_.ariaSort}>{num_.node}</th>
                <th scope="col">Subjek</th>
                {show('status') && <th scope="col">Status</th>}
                {show('priority') && <th scope="col">Prioritas</th>}
                {show('channel') && <th scope="col">Kategori</th>}
                {show('channel') && <th scope="col">Channel</th>}
                {show('owner') && <th scope="col">PIC</th>}
                {show('received') && <th scope="col" aria-sort={recvAt.ariaSort}>{recvAt.node}</th>}
                {show('updated') && <th scope="col" aria-sort={upd.ariaSort}>{upd.node}</th>}
                <th scope="col" aria-sort={recv.ariaSort}>{recv.node}</th>
                {params.rowActions && <th scope="col"><span className="sr-only">Aksi</span></th>}
              </tr>
            </thead>
            <tbody>
              {items.map((t) => (
                <tr key={t.id}>
                  <td><button type="button" className="tno linkish" data-open-ticket={t.id} onClick={() => onOpen(t)}>{t.displayNumber}</button></td>
                  <td className="subj">
                    <span className="tsub" title={t.subject}>{t.subject}</span>
                    <span className="tmeta">siklus {t.cycleNo}{t.relatedWork.open > 0 ? ` · ${num(t.relatedWork.open)} pekerjaan terkait` : ''}</span>
                  </td>
                  {show('status') && <td><Badge tone={statusTone(t.status)}>{t.status.label}</Badge></td>}
                  {show('priority') && <td className="nowrap">{t.priority
                    ? <span className={`pri ${priorityDot(t.priority.code)}`}><b aria-hidden="true" />{t.priority.label}</span>
                    : <span className="muted">—</span>}</td>}
                  {show('channel') && <td className="wrapcell">{t.category?.label ?? <span className="muted">Tanpa kategori</span>}</td>}
                  {show('channel') && <td className="wrapcell muted-cell">{t.channel?.label ?? <span className="muted">Tanpa kanal</span>}</td>}
                  {show('owner') && <td className="nowrap">{t.owner
                    ? <span className="cell-pic"><span className="cell-av" aria-hidden="true">{initials(t.owner.label)}</span>{t.owner.label}</span>
                    : <span className="chip-tag">Belum ada</span>}</td>}
                  {show('received') && <td className="nowrap muted-cell" title={instantLabel(t.receivedAt, tz)}>{shortInstant(t.receivedAt, tz)}</td>}
                  {show('updated') && <td className="nowrap muted-cell" title={instantLabel(t.updatedAt, tz)}>{shortInstant(t.updatedAt, tz)}</td>}
                  <td className="nowrap"><b>{ageLabel(t.ageSeconds)}</b></td>
                  {params.rowActions && <td><StatusButton ticket={t} onClick={() => onTransition(t)} compact /></td>}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {list.total > 0 && (
        <div className="ppager">
          <span>Menampilkan {num(range.from)}–{num(range.to)} dari {num(list.total)} tiket</span>
          {more.status === 'error' && <span className="field-error tiny" role="alert">Halaman berikutnya gagal dimuat. {errorMessage(more.error)}</span>}
          <span className="r">
            <Button className="btn-small" onClick={() => show_(index - 1)} disabled={index === 0}>Sebelumnya</Button>
            <Button className="btn-small" onClick={next} disabled={(!pages[index + 1] && !page.next) || more.status === 'loading'}
              aria-busy={more.status === 'loading'}>
              {more.status === 'loading' ? 'Memuat…' : 'Berikutnya'}
            </Button>
          </span>
        </div>
      )}
      <div className="bd basis-foot">
        <BasisNote basis={list.basis} total={list.total} resolutionEvents={list.resolutionEvents} />
      </div>
    </>
  );
}

/** Opens the same transition dialog as drag. Archived (closed) tickets are read-only. */
function StatusButton({ ticket, onClick, compact }: { ticket: TicketListItem; onClick: () => void; compact?: boolean }) {
  if (ticket.status.archived) return <span className="tiny muted">Arsip read-only</span>;
  if (ticket.allowedActions.length === 0) return <span className="tiny muted">Tanpa aksi status</span>;
  return (
    <Button className={compact ? 'btn-small' : undefined} onClick={onClick}
      aria-label={`Ubah status ${ticket.displayNumber}`}>Ubah status</Button>
  );
}

interface ColumnState {
  items: TicketListItem[];
  cursor: string | null;
  status: 'idle' | 'loading' | 'error';
  error?: unknown;
}

/**
 * O03: columns are the statuses in scope with the server's whole-column total; each column
 * pages on its own through the List API (`status=<code>` + same filters). Drag is a desktop
 * shortcut to the same dialog as the "Ubah status" button; nothing moves before the server agrees.
 */
function BoardView({ api, workspace, board, apiQuery, filtered, canCreate, onOpen, onTransition, onRefused, onReset, onCreate }: {
  api: Api;
  workspace: Workspace;
  board: TicketBoard;
  apiQuery: string;
  filtered: boolean;
  canCreate: boolean;
  onOpen: (t: TicketListItem) => void;
  onTransition: (t: TicketListItem, to: TicketStatusCode | null) => void;
  onRefused: (message: string) => void;
  onReset: () => void;
  onCreate: () => void;
}) {
  const [cols, setCols] = useState<Record<string, ColumnState>>(() =>
    Object.fromEntries(board.columns.map((c) => [c.status.code, { items: c.items, cursor: c.nextCursor, status: 'idle' }])));
  const [dragging, setDragging] = useState<TicketListItem | null>(null);
  const [over, setOver] = useState<string | null>(null);
  const ctls = useRef(new Map<string, AbortController>());
  useEffect(() => () => ctls.current.forEach((c) => c.abort()), []);
  const canDrag = useFinePointer();

  async function loadMore(code: TicketStatusCode) {
    const col = cols[code];
    if (!col?.cursor) return;
    ctls.current.get(code)?.abort();
    const c = new AbortController();
    ctls.current.set(code, c);
    setCols((s) => ({ ...s, [code]: { ...s[code]!, status: 'loading' } }));
    const q = new URLSearchParams(apiQuery);
    q.set('status', code);
    try {
      const page = await api.listTickets(workspace.id, q.toString(), col.cursor, c.signal);
      setCols((s) => {
        const old = s[code]!;
        const seen = new Set(old.items.map((t) => t.id));
        return { ...s, [code]: { items: [...old.items, ...page.items.filter((t) => !seen.has(t.id))], cursor: page.nextCursor, status: 'idle' } };
      });
    } catch (error) {
      if (!isAbort(error)) setCols((s) => ({ ...s, [code]: { ...s[code]!, status: 'error', error } }));
    }
  }

  function drop(e: DragEvent, to: TicketStatusCode) {
    e.preventDefault();
    setOver(null);
    const t = dragging;
    setDragging(null);
    if (!t) return;
    const plan = planDrop(t, to);
    if (plan.kind === 'ignore') return;
    if (plan.kind === 'refuse') {
      onRefused(plan.message);
      return;
    }
    // Resolve/reopen need a mandatory note, so the same dialog opens with the target preselected.
    onTransition(t, plan.to);
  }

  if (board.total === 0) {
    return filtered
      ? <EmptyState title="Tidak ada tiket sesuai filter" body="Ubah atau reset filter untuk melihat tiket lain."
        action={<Button onClick={onReset}>Reset filter</Button>} />
      : <EmptyState title="Belum ada tiket" body={`Belum ada tiket di ${workspace.code}.`}
        action={canCreate ? <Button variant="primary" onClick={onCreate}>＋ Buat tiket</Button> : undefined} />;
  }
  return (
    <>
      <p className="hintline board-hint">
        {canDrag ? <>Seret kartu ke kolom lain, atau pakai tombol “Ubah status” (keyboard).</> : <>Pakai tombol “Ubah status” pada kartu.</>}
        {' '}Keduanya membuka dialog yang sama; status baru tersimpan hanya setelah server menyetujui.
      </p>
      <div className="board" role="region" aria-label="Board status" tabIndex={0}>
        {board.columns.map((c) => {
          const s = cols[c.status.code]!;
          const headingId = `col-${c.status.code}`;
          return (
            <section key={c.status.code} className={`column${over === c.status.code ? ' drop' : ''}`} aria-labelledby={headingId}
              onDragOver={canDrag ? (e) => { if (dragging) { e.preventDefault(); setOver(c.status.code); } } : undefined}
              onDragLeave={canDrag ? () => setOver((o) => (o === c.status.code ? null : o)) : undefined}
              onDrop={canDrag ? (e) => drop(e, c.status.code) : undefined}>
              <h3 id={headingId}>
                <span className={`col-label tone-${statusTone(c.status)}`}>{c.status.label}</span>
                <span className="n" aria-label={`${num(c.total)} tiket`}>{num(c.total)}</span>
              </h3>
              {c.status.archived && <p className="tiny muted">Arsip historis, read-only.</p>}
              <div className="col-list">
                {s.items.length === 0 && <p className="col-empty">Kosong</p>}
                {s.items.map((t) => (
                  <TicketCard key={t.id} ticket={t} tz={workspace.timezone} draggable={canDrag && !t.status.archived && t.allowedActions.length > 0}
                    onDragStart={() => setDragging(t)} onDragEnd={() => { setDragging(null); setOver(null); }}
                    onOpen={() => onOpen(t)} onTransition={() => onTransition(t, null)} />
                ))}
              </div>
              <div className="col-foot">
                <span className="tiny muted">{num(s.items.length)} dari {num(c.total)}</span>
                {s.status === 'error' && (
                  <span className="field-error tiny" role="alert">Gagal memuat. {errorMessage(s.error)}</span>
                )}
                {s.cursor && (
                  <Button className="btn-small" onClick={() => loadMore(c.status.code)} disabled={s.status === 'loading'}
                    aria-label={`Muat tiket ${c.status.label} berikutnya`}>
                    {s.status === 'loading' ? 'Memuat…' : s.status === 'error' ? 'Coba lagi' : 'Muat lagi'}
                  </Button>
                )}
              </div>
            </section>
          );
        })}
      </div>
      <BasisNote basis={board.basis} total={board.total} resolutionEvents={board.resolutionEvents} />
    </>
  );
}

function useFinePointer(): boolean {
  const query = '(pointer: fine) and (min-width: 761px)';
  const [fine, setFine] = useState(() => typeof window !== 'undefined' && !!window.matchMedia?.(query).matches);
  useEffect(() => {
    const m = window.matchMedia?.(query);
    if (!m) return;
    const on = () => setFine(m.matches);
    m.addEventListener('change', on);
    return () => m.removeEventListener('change', on);
  }, []);
  return fine;
}

/** C11: number, subject (max 3 lines), priority, PIC, age, related work; no PII. No button in a button. */
function TicketCard({ ticket: t, tz, draggable, onDragStart, onDragEnd, onOpen, onTransition }: {
  ticket: TicketListItem;
  tz: string;
  draggable: boolean;
  onDragStart: () => void;
  onDragEnd: () => void;
  onOpen: () => void;
  onTransition: () => void;
}) {
  return (
    <article className="ticket tcard" draggable={draggable} aria-label={`${t.displayNumber}: ${t.subject}`}
      onDragStart={draggable ? (e) => { e.dataTransfer.setData('text/plain', t.id); e.dataTransfer.effectAllowed = 'move'; onDragStart(); } : undefined}
      onDragEnd={draggable ? onDragEnd : undefined}>
      <button type="button" className="ticket-open" data-open-ticket={t.id} onClick={onOpen}>
        <span className="tc-no">
          <span>{t.displayNumber}</span>
          {t.priority && <span className={`pri ${priorityDot(t.priority.code)}`}><b aria-hidden="true" />{t.priority.label}</span>}
        </span>
        <span className="ticket-title">{t.subject}</span>
      </button>
      {t.relatedWork.open > 0 && (
        <div className="chips">
          <Badge tone={t.relatedWork.blockingOpen > 0 ? 'waiting' : 'neutral'}>
            {`${t.relatedWork.open} pekerjaan terkait${t.relatedWork.blockingOpen > 0 ? ` · ${t.relatedWork.blockingOpen} memblokir` : ''}`}
          </Badge>
        </div>
      )}
      <div className="ticket-meta">
        {t.owner ? <span>{t.owner.label}</span> : <span className="no-pic">belum ada PIC</span>}
        {' · '}<span title={instantLabel(t.receivedAt, tz)}>{ageLabel(t.ageSeconds)}</span>
      </div>
      <StatusButton ticket={t} onClick={onTransition} compact />
    </article>
  );
}

const BLOCK_TEXT: Record<string, string> = {
  blocking_related_work: 'Masih ada pekerjaan terkait yang memblokir penyelesaian. Selesaikan atau lepaskan dulu pekerjaan itu.',
};

/**
 * C12: target options are the server's `allowedActions`; blocked ones stay visible but disabled
 * with the reason. Submit sends the version the user saw. Nothing changes on screen before the
 * server answers; on 409/403/422/400 the dialog stays open with the input kept.
 */
function TransitionDialog({ api, workspace, ticket, initialTo, onClose, onDone, onReloadList }: {
  api: Api;
  workspace: Workspace;
  ticket: TicketListItem;
  initialTo: TicketStatusCode | null;
  onClose: () => void;
  onDone: (t: TicketListItem, message: string) => void;
  onReloadList: () => void;
}) {
  const actions = ticket.allowedActions;
  const firstOk = actions.find((a) => !a.blockedBy)?.to.code ?? '';
  const [to, setTo] = useState<string>(initialTo ?? firstOk);
  const [summary, setSummary] = useState('');
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<{ title: string; body: string; conflict: boolean; field?: 'summary' | 'reason' } | null>(null);
  const id = useId();
  const action: TicketAllowedAction | undefined = actions.find((a) => a.to.code === to);
  const needs = action?.requires ?? 'none';
  const text = needs === 'summary' ? summary : needs === 'reason' ? reason : '';
  const missing = needs !== 'none' && text.trim() === '';

  async function submit() {
    if (!action || action.blockedBy || busy) return;
    if (missing) {
      setError({ title: 'Isian wajib belum diisi', body: needs === 'summary' ? 'Ringkasan penyelesaian wajib diisi.' : 'Alasan membuka kembali wajib diisi.', conflict: false, field: needs === 'summary' ? 'summary' : 'reason' });
      document.getElementById(`${id}-${needs}`)?.focus();
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await api.transitionTicket(workspace.id, ticket.id, {
        to: action.to.code,
        version: ticket.version,
        ...(needs === 'summary' ? { summary: summary.trim() } : {}),
        ...(needs === 'reason' ? { reason: reason.trim() } : {}),
      });
      onDone(res.ticket, `${res.ticket.displayNumber} sekarang ${res.ticket.status.label}. Perubahan tersimpan dan tercatat di audit.`);
    } catch (e) {
      setBusy(false);
      setError(transitionError(e));
    }
  }

  return (
    <Dialog open onClose={busy ? () => undefined : onClose} title="Ubah status tiket" initialFocus={`#${CSS.escape(`${id}-to`)}`}
      footer={(
        <>
          <Button onClick={onClose} disabled={busy}>Batal</Button>
          <Button variant="primary" onClick={submit} disabled={busy || !action || !!action.blockedBy} aria-busy={busy}>
            {busy ? 'Menyimpan…' : action?.kind === 'resolve' ? 'Selesaikan tiket' : action?.kind === 'reopen' ? 'Buka kembali' : 'Simpan status'}
          </Button>
        </>
      )}>
      <p className="muted">{ticket.displayNumber} · {ticket.subject}</p>
      <p>
        Dari <Badge tone={statusTone(ticket.status)}>{ticket.status.label}</Badge>
        {action && <> ke <Badge tone={statusTone(action.to)}>{action.to.label}</Badge></>}
      </p>
      {error && (
        <div className="state-error spaced-bottom" role="alert">
          <h3>{error.title}</h3>
          <p>{error.body}</p>
          {error.conflict && <Button onClick={() => { onReloadList(); onClose(); }}>Muat ulang data</Button>}
        </div>
      )}
      {actions.length === 0 ? (
        <p>Tidak ada perubahan status yang boleh Anda lakukan pada tiket ini sekarang.</p>
      ) : (
        <form className="formgrid" onSubmit={(e) => { e.preventDefault(); void submit(); }}>
          <div className="field full">
            <label htmlFor={`${id}-to`}>Status tujuan</label>
            <select id={`${id}-to`} className="field-control" value={to} disabled={busy}
              onChange={(e) => { setTo(e.target.value); setError(null); }}
              aria-describedby={action?.blockedBy ? `${id}-blocked` : undefined}>
              {!action && <option value="">Pilih status</option>}
              {actions.map((a) => (
                <option key={a.to.code} value={a.to.code}>
                  {a.to.label}{a.blockedBy ? ' (diblokir)' : ''}
                </option>
              ))}
            </select>
            {action?.blockedBy && <span id={`${id}-blocked`} className="field-error">{BLOCK_TEXT[action.blockedBy] ?? 'Aksi ini sedang diblokir.'}</span>}
          </div>
          {needs === 'summary' && (
            <div className="field full">
              <label htmlFor={`${id}-summary`}>Ringkasan penyelesaian (wajib)</label>
              <textarea id={`${id}-summary`} className="field-control" value={summary} maxLength={5000} disabled={busy}
                aria-invalid={error?.field === 'summary' ? true : undefined}
                onChange={(e) => setSummary(e.target.value)} />
              <span className="field-hint">Disimpan di riwayat penyelesaian siklus ini.</span>
            </div>
          )}
          {needs === 'reason' && (
            <div className="field full">
              <label htmlFor={`${id}-reason`}>Alasan membuka kembali (wajib)</label>
              <textarea id={`${id}-reason`} className="field-control" value={reason} maxLength={5000} disabled={busy}
                aria-invalid={error?.field === 'reason' ? true : undefined}
                onChange={(e) => setReason(e.target.value)} />
              <span className="field-hint">Membuka kembali memulai siklus penanganan baru (status Ditangani).</span>
            </div>
          )}
          {needs === 'none' && action && (
            <p className="tiny muted full">Pindah status biasa tidak memerlukan catatan. Tutup final (arsip) tidak tersedia.</p>
          )}
          <button type="submit" hidden tabIndex={-1} aria-hidden="true" />
        </form>
      )}
    </Dialog>
  );
}

function transitionError(e: unknown): { title: string; body: string; conflict: boolean } {
  if (isSessionReloadNeeded(e)) return { title: SESSION_RELOAD_TITLE, body: sessionReloadText(e.requestId), conflict: false };
  if (e instanceof ApiError) {
    const rid = e.requestId ? ` (ID permintaan ${e.requestId})` : '';
    switch (e.status) {
      case 429:
        return { title: 'Terlalu banyak permintaan', body: `${rateLimitText(e)} Status belum berubah; isian Anda tetap di sini.${rid}`, conflict: false };
      case 409:
        return { title: 'Tiket sudah diubah pihak lain', body: `Status tidak disimpan. Isian Anda tetap di sini; muat ulang data lalu coba lagi.${rid}`, conflict: true };
      case 403:
        return { title: 'Tidak diizinkan', body: `Anda tidak memiliki izin untuk perubahan ini. Tidak ada yang berubah.${rid}`, conflict: false };
      case 422:
        return { title: 'Ditolak aturan proses', body: `Perubahan tidak sesuai aturan lifecycle atau masih diblokir. Tidak ada yang berubah.${rid}`, conflict: true };
      case 400:
        return { title: 'Isian tidak valid', body: `Periksa isian lalu coba lagi. Tidak ada yang berubah.${rid}`, conflict: false };
      case 404:
        return { title: 'Tiket tidak ditemukan', body: `Tiket ini tidak ada atau tidak dapat Anda akses.${rid}`, conflict: true };
      case 401:
        return { title: 'Sesi berakhir', body: 'Muat ulang halaman untuk masuk lagi. Tidak ada yang berubah.', conflict: false };
      default:
        return { title: 'Belum tersimpan', body: `${errorMessage(e)} Status belum berubah; coba lagi.${rid}`, conflict: false };
    }
  }
  return { title: 'Belum tersimpan', body: 'Terjadi kesalahan tak terduga. Status belum berubah.', conflict: false };
}

function TicketSkeleton({ view }: { view: TicketView }) {
  return (
    <div role="status" aria-live="polite">
      <span className="sr-only">Memuat {view === 'list' ? 'daftar tiket' : 'board'}…</span>
      {view === 'board'
        ? <div className="board" aria-hidden="true">{[0, 1, 2, 3].map((i) => <div key={i} className="column"><div className="skeleton" /><div className="skeleton" /></div>)}</div>
        : [0, 1, 2, 3, 4].map((i) => <div key={i} className="skeleton" aria-hidden="true" />)}
    </div>
  );
}

function TicketError({ error, onRetry, onReset }: { error: unknown; onRetry: () => void; onReset: (() => void) | null }) {
  if (error instanceof ApiError && error.status === 400 && onReset) {
    return <ErrorState title="Filter tidak valid untuk perusahaan ini"
      body="Nilai filter pada alamat halaman tidak dikenali di perusahaan Anda. Tidak ada tiket yang ditampilkan."
      requestId={error.requestId} onRetry={onReset} retryLabel="Reset filter" />;
  }
  if (error instanceof ApiError && error.status === 403) return <ForbiddenState />;
  if (error instanceof ApiError && error.status === 404) return <NotFoundState />;
  if (error instanceof ApiError && error.status === 401) {
    return <ErrorState title="Sesi berakhir" body="Sesi uji tidak lagi aktif. Muat ulang halaman untuk masuk lagi." requestId={error.requestId} />;
  }
  return <ErrorState title="Tiket belum dapat dimuat"
    body={`${errorMessage(error)} Tidak ada data lama yang dipakai.`}
    requestId={error instanceof ApiError ? error.requestId : null} onRetry={onRetry} />;
}

function errorMessage(e: unknown): string {
  if (e instanceof ApiError) return e.code === 'rate_limited' ? rateLimitText(e) : e.code === 'network' ? 'Server tidak dapat dihubungi.' : e.message;
  return 'Terjadi kesalahan tak terduga.';
}

/**
 * C05 column chooser: allowlisted list columns; number, subject and age always stay. CP-UI2b: the
 * default is the prototype's set (Diperbarui off) and the per-row "Ubah status" button is opt-in.
 */
function ColumnChooser({ params, onApply }: { params: TicketParams; onApply: (p: TicketParams) => void }) {
  const changed = !isDefaultColumns(params.columns) || params.rowActions;
  const toggle = (c: TicketColumn, on: boolean) => {
    const next = on ? [...params.columns, c] : params.columns.filter((x) => x !== c);
    if (next.length === 0) return;
    onApply({ ...params, columns: canonicalColumns(next) });
  };
  return (
    <Popover label="Kolom daftar" title="Kolom daftar"
      trigger={<>Kolom{changed && <span className="count-badge" aria-label="diubah dari bawaan">•</span>}</>}>
      {(close) => (
        <>
          <fieldset className="column-checks">
            <legend className="tiny muted">Nomor, subjek, dan umur selalu tampil. Minimal satu kolom lain.</legend>
            {TICKET_COLUMNS.map((c) => {
              const on = params.columns.includes(c);
              const last = on && params.columns.length === 1;
              return (
                <label key={c} className="row check">
                  <input type="checkbox" checked={on} disabled={last} onChange={(e) => toggle(c, e.target.checked)} />
                  {COLUMN_LABELS[c]}{last && <span className="sr-only"> (kolom terakhir, tidak bisa disembunyikan)</span>}
                </label>
              );
            })}
            <label className="row check">
              <input type="checkbox" checked={params.rowActions} onChange={(e) => onApply({ ...params, rowActions: e.target.checked })} />
              Tombol ubah status per baris
            </label>
          </fieldset>
          <div className="popover-foot row between">
            <Button variant="ghost" onClick={() => onApply({ ...params, columns: [...DEFAULT_TICKET_COLUMNS], rowActions: false })}
              disabled={!changed}>Kembali ke bawaan</Button>
            <Button variant="primary" onClick={() => close()}>Selesai</Button>
          </div>
        </>
      )}
    </Popover>
  );
}
