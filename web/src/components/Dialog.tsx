import { useEffect, useId, useRef, type ReactNode, type RefObject } from 'react';
import { Button } from './Button';
import { Icon } from './Icon';

/**
 * Native modal <dialog>: the rest of the page is inert while open, Escape closes, and focus
 * returns to whatever opened it. `initialFocus` picks the first focused element (C31: Cancel
 * for high-impact confirmations).
 */
export function Dialog({ open, onClose, title, children, footer, variant, initialFocus, returnFocus }: {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  variant?: 'drawer';
  initialFocus?: string;
  /** Element to focus on close instead of the opener (e.g. the control that asked for it). */
  returnFocus?: RefObject<HTMLElement | null>;
}) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    if (open && !el.open) {
      const opener = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      el.showModal();
      const target = initialFocus ? el.querySelector<HTMLElement>(initialFocus) : null;
      target?.focus();
      return () => {
        if (el.open) el.close();
        const back = returnFocus?.current;
        if (back?.isConnected) back.focus();
        else if (opener?.isConnected) opener.focus();
      };
    }
    return undefined;
  }, [open, initialFocus]);

  return (
    <dialog ref={ref} className={variant} aria-labelledby={titleId}
      onCancel={(e) => { e.preventDefault(); onClose(); }}>
      {open && (
        <>
          <div className="dialog-head">
            <h2 id={titleId}>{title}</h2>
            <Button variant="ghost" iconOnly aria-label="Tutup" onClick={onClose}><Icon name="close" /></Button>
          </div>
          <div className="dialog-body">{children}</div>
          {footer && <div className="dialog-foot">{footer}</div>}
        </>
      )}
    </dialog>
  );
}
