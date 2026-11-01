import type { ButtonHTMLAttributes } from 'react';

type Variant = 'primary' | 'secondary' | 'ghost';
interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  iconOnly?: boolean;
}

export function Button({ variant = 'secondary', iconOnly = false, className, type = 'button', ...rest }: Props) {
  const cls = ['btn', variant !== 'secondary' && `btn-${variant}`, iconOnly && 'btn-icon', className]
    .filter(Boolean)
    .join(' ');
  return <button type={type} className={cls} {...rest} />;
}
