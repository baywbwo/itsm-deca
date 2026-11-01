import { useEffect, useId, useLayoutEffect, useRef, useState, type ReactNode } from 'react';

/**
 * Non-modal popover (C05 filter / C32 saved view). The trigger carries `aria-expanded` and
 * `aria-controls`; opening moves focus to the first control inside; Escape or "close" returns
 * focus to the trigger; a click outside closes without stealing focus. Below 640 px the panel
 * is shown as a bottom sheet (CSS). Content stays mounted only while open.
 */
export function Popover({ label, trigger, title, children, className, open: controlled, onOpenChange }: {
  /** Accessible name of the panel. */
  label: string;
  /** Visible trigger content (text plus optional badge). */
  trigger: ReactNode;
  /** Optional visible heading of the panel. */
  title?: string;
  children: (close: () => void) => ReactNode;
  className?: string;
  open?: boolean;
  onOpenChange?: (open: boolean) => void;
}) {
  const [own, setOwn] = useState(false);
  const open = controlled ?? own;
  const setOpen = (v: boolean) => {
    if (controlled === undefined) setOwn(v);
    onOpenChange?.(v);
  };
  const id = useId();
  const wrap = useRef<HTMLDivElement>(null);
  const button = useRef<HTMLButtonElement>(null);
  const panel = useRef<HTMLDivElement>(null);
  const close = (refocus = true) => {
    setOpen(false);
    if (refocus) button.current?.focus();
  };
  const closeRef = useRef(close);
  closeRef.current = close;

  // Keep the anchored panel inside the viewport (the bottom sheet below 640 px needs no shift).
  useLayoutEffect(() => {
    const el = panel.current;
    if (!open || !el || window.innerWidth <= 640) return;
    el.style.transform = '';
    const r = el.getBoundingClientRect();
    const gap = 8;
    let shift = Math.min(0, document.documentElement.clientWidth - gap - r.right);
    if (r.left + shift < gap) shift = gap - r.left;
    if (shift) el.style.transform = `translateX(${Math.round(shift)}px)`;
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const body = panel.current?.querySelector('.popover-body');
    const first = body?.querySelector<HTMLElement>(
      'input, select, textarea, button, [href], [tabindex]:not([tabindex="-1"])',
    );
    (first ?? panel.current)?.focus();
    const onDown = (e: PointerEvent) => {
      if (wrap.current && !wrap.current.contains(e.target as Node)) closeRef.current(false);
    };
    document.addEventListener('pointerdown', onDown);
    return () => document.removeEventListener('pointerdown', onDown);
  }, [open]);

  return (
    <div className={['popover', className].filter(Boolean).join(' ')} ref={wrap}
      onKeyDown={(e) => {
        if (e.key === 'Escape' && open) {
          e.stopPropagation();
          close();
        }
      }}>
      <button ref={button} type="button" className={open ? 'btn popover-trigger open' : 'btn popover-trigger'}
        aria-expanded={open} aria-controls={id} aria-haspopup="dialog" onClick={() => (open ? close() : setOpen(true))}>
        {trigger}
      </button>
      {open && (
        <div id={id} ref={panel} className="popover-panel" role="dialog" aria-label={label} tabIndex={-1}>
          {title && (
            <div className="popover-head row between">
              <strong>{title}</strong>
              <button type="button" className="btn btn-ghost btn-icon" aria-label={`Tutup ${label}`}
                onClick={() => close()}>✕</button>
            </div>
          )}
          <div className="popover-body">{children(() => close())}</div>
        </div>
      )}
    </div>
  );
}
