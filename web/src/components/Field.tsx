import { useId, type InputHTMLAttributes, type ReactNode, type SelectHTMLAttributes } from 'react';

interface Common {
  label: string;
  hint?: string | undefined;
  error?: string | undefined;
  className?: string | undefined;
}

function describedBy(id: string, hint?: string, error?: string) {
  const ids = [hint && `${id}-hint`, error && `${id}-error`].filter(Boolean).join(' ');
  return ids || undefined;
}

function Shell({ id, label, hint, error, className, children }: Common & { id: string; children: ReactNode }) {
  return (
    <div className={['field', className].filter(Boolean).join(' ')}>
      <label htmlFor={id}>{label}</label>
      {children}
      {hint && <span id={`${id}-hint`} className="field-hint">{hint}</span>}
      {error && <span id={`${id}-error`} className="field-error">{error}</span>}
    </div>
  );
}

export function TextField({ label, hint, error, className, ...input }: Common & InputHTMLAttributes<HTMLInputElement>) {
  const id = useId();
  return (
    <Shell id={id} label={label} hint={hint} error={error} className={className}>
      <input id={id} className="field-control" aria-invalid={error ? true : undefined}
        aria-describedby={describedBy(id, hint, error)} {...input} />
    </Shell>
  );
}

export function SelectField({ label, hint, error, className, children, ...select }: Common & SelectHTMLAttributes<HTMLSelectElement>) {
  const id = useId();
  return (
    <Shell id={id} label={label} hint={hint} error={error} className={className}>
      <select id={id} className="field-control" aria-invalid={error ? true : undefined}
        aria-describedby={describedBy(id, hint, error)} {...select}>
        {children}
      </select>
    </Shell>
  );
}
