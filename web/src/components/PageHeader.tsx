import type { ReactNode } from 'react';

/** C03: optional eyebrow, one h1, description, at most one primary action. */
export function PageHeader({ eyebrow, title, description, action }: {
  eyebrow?: string;
  title: string;
  description: string;
  action?: ReactNode;
}) {
  return (
    <div className="intro">
      {eyebrow && <div className="eyebrow">{eyebrow}</div>}
      <div className="intro-row">
        <h1 tabIndex={-1} id="page-title">{title}</h1>
        {action}
      </div>
      <p>{description}</p>
    </div>
  );
}
