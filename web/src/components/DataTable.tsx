import type { ReactNode } from 'react';
import { EmptyState } from './States';

export interface Column<T> {
  key: string;
  header: string;
  cell: (row: T) => ReactNode;
}

/** C08 foundation: semantic table, overflow inside its own container, explicit empty state. */
export function DataTable<T>({ caption, columns, rows, rowKey, empty }: {
  caption: string;
  columns: readonly Column<T>[];
  rows: readonly T[];
  rowKey: (row: T) => string;
  empty: { title: string; body: string };
}) {
  if (rows.length === 0) return <EmptyState title={empty.title} body={empty.body} />;
  return (
    <div className="tablebox" role="region" aria-label={caption} tabIndex={0}>
      <table>
        <caption>{caption}</caption>
        <thead>
          <tr>{columns.map((c) => <th key={c.key} scope="col">{c.header}</th>)}</tr>
        </thead>
        <tbody>
          {rows.map((r) => (
            <tr key={rowKey(r)}>{columns.map((c) => <td key={c.key}>{c.cell(r)}</td>)}</tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
