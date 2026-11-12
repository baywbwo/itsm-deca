import type { ReactNode } from 'react';
import { Button } from './Button';

export function LoadingState({ label, rows = 4 }: { label: string; rows?: number }) {
  return (
    <div role="status" aria-live="polite">
      <p className="muted">{label}</p>
      {Array.from({ length: rows }, (_, i) => <div key={i} className="skeleton" aria-hidden="true" />)}
    </div>
  );
}

export function EmptyState({ title, body, action }: { title: string; body: string; action?: ReactNode }) {
  return (
    <div className="state">
      <h2>{title}</h2>
      <p className="muted">{body}</p>
      {action}
    </div>
  );
}

export function ErrorState({ title, body, onRetry, retryLabel = 'Coba lagi', requestId }: {
  title: string;
  body: string;
  onRetry?: (() => void) | undefined;
  retryLabel?: string;
  requestId?: string | null | undefined;
}) {
  return (
    <div className="state-error" role="alert">
      <h2>{title}</h2>
      <p>{body}</p>
      {requestId && <p className="tiny">ID permintaan: {requestId}</p>}
      {onRetry && <Button onClick={onRetry}>{retryLabel}</Button>}
    </div>
  );
}

/** 403 inside a visible workspace: says nothing about the hidden data. */
export function ForbiddenState() {
  return (
    <EmptyState title="Akses ditolak"
      body="Anda tidak memiliki izin untuk halaman ini di perusahaan Anda. Hubungi pengelola perusahaan bila akses diperlukan." />
  );
}

/** 404: unknown, foreign and malformed IDs look identical (no enumeration). */
export function NotFoundState({ action }: { action?: ReactNode }) {
  return <EmptyState title="Perusahaan tidak ditemukan" body="Perusahaan ini tidak ada atau tidak dapat Anda akses." action={action} />;
}
