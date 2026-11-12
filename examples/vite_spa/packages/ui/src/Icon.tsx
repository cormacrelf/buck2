import { type IconName, icons } from './icons/index.ts';
import styles from './Icon.module.css';

export interface IconProps {
  name: IconName;
  /** Accessible label. Without one the icon is decorative. */
  label?: string;
  size?: number;
  className?: string;
}

export function Icon({ name, label, size = 18, className }: IconProps) {
  return (
    <span
      className={className ? `${styles.icon} ${className}` : styles.icon}
      style={{ width: size, height: size }}
      role={label ? 'img' : undefined}
      aria-label={label}
      aria-hidden={label ? undefined : true}
      // The markup is our own SVG files, imported at build time.
      dangerouslySetInnerHTML={{ __html: icons[name] }}
    />
  );
}
