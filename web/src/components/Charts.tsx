import { dayLabel, num } from '../app/format';

export interface BarItem {
  key: string;
  label: string;
  value: number;
}

/** Horizontal bars from zero against `total`; the numbers are always printed next to them. */
export function BarList({ items, total, label }: { items: readonly BarItem[]; total: number; label: string }) {
  const summary = items.map((i) => `${i.label} ${num(i.value)}`).join(', ');
  return (
    <div className="bars" role="img" aria-label={`${label}: ${summary}`}>
      {items.map((i) => (
        <div className="barrow" key={i.key}>
          <span>{i.label}</span>
          <div className="track"><div className="fill" style={{ width: `${total > 0 ? (i.value / total) * 100 : 0}%` }} /></div>
          <b>{num(i.value)}</b>
        </div>
      ))}
    </div>
  );
}

export interface TrendPoint {
  date: string;
  received: number;
  resolved: number;
}

const W = 640;
const H = 180;
const LEFT = 32;
const BOTTOM = 22;
const TOP = 8;

/** Daily received vs resolved as paired columns on a zero baseline; a table carries the numbers. */
export function TrendChart({ points, label }: { points: readonly TrendPoint[]; label: string }) {
  const max = Math.max(1, ...points.map((p) => Math.max(p.received, p.resolved)));
  const plotW = W - LEFT;
  const plotH = H - BOTTOM - TOP;
  const step = plotW / Math.max(1, points.length);
  const bar = Math.max(2, Math.min(18, step * 0.36));
  const y = (v: number) => TOP + plotH - (v / max) * plotH;
  const ticks = new Set([0, Math.floor((points.length - 1) / 2), points.length - 1]);
  const received = points.reduce((s, p) => s + p.received, 0);
  const resolved = points.reduce((s, p) => s + p.resolved, 0);
  return (
    <>
      <div className="legend tiny" aria-hidden="true">
        <span><i className="key-dot key-received" />Diterima</span>
        <span><i className="key-dot key-resolved" />Terselesaikan</span>
      </div>
      <svg className="trend" viewBox={`0 0 ${W} ${H}`} role="img"
        aria-label={`${label}: diterima ${num(received)}, terselesaikan ${num(resolved)} dalam ${points.length} hari`}>
        <text x={LEFT - 6} y={y(max) + 4} textAnchor="end" className="axis">{num(max)}</text>
        <text x={LEFT - 6} y={y(0) + 4} textAnchor="end" className="axis">0</text>
        <line x1={LEFT} x2={W} y1={y(max)} y2={y(max)} className="gridline" />
        {points.map((p, i) => {
          const cx = LEFT + step * i + step / 2;
          return (
            <g key={p.date}>
              <rect x={cx - bar - 1} y={y(p.received)} width={bar} height={y(0) - y(p.received)} className="col-received" />
              <rect x={cx + 1} y={y(p.resolved)} width={bar} height={y(0) - y(p.resolved)} className="col-resolved" />
              {ticks.has(i) && <text x={cx} y={H - 6} textAnchor="middle" className="axis">{dayLabel(p.date, true)}</text>}
            </g>
          );
        })}
        <line x1={LEFT} x2={W} y1={y(0)} y2={y(0)} className="baseline" />
      </svg>
    </>
  );
}
