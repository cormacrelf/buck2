import type { ButtonHTMLAttributes, ReactNode } from 'react';
import styles from './Button.module.css';
import { Icon } from './Icon.tsx';
import type { IconName } from './icons/index.ts';

export type ButtonVariant = 'primary' | 'quiet' | 'danger';

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: ButtonVariant;
  icon?: IconName;
  pressed?: boolean;
  children?: ReactNode;
}

export function Button({
  variant = 'quiet',
  icon,
  pressed,
  className,
  children,
  type = 'button',
  ...rest
}: ButtonProps) {
  const classes = [styles.button, styles[variant], className].filter(Boolean).join(' ');
  return (
    <button {...rest} type={type} className={classes} aria-pressed={pressed}>
      {icon && <Icon name={icon} size={16} />}
      {children}
    </button>
  );
}
