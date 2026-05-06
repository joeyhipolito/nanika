import type { CSSProperties } from 'react';

import { iconRegistry, type IconName } from './registry';

export type IconVariant = 'line' | 'duotone';

export interface IconProps {
  name: IconName;
  variant?: IconVariant;
  size?: number;
  className?: string;
  label?: string;
  style?: CSSProperties;
}

const ACTIVE_SUFFIX = 'Active';

function resolveName(name: IconName, variant: IconVariant): IconName {
  if (variant === 'line') return name;
  const candidate = `${name}${ACTIVE_SUFFIX}` as IconName;
  return candidate in iconRegistry ? candidate : name;
}

/**
 * Renders a named icon from the registry.
 *
 * Duotone icons read `var(--icon-secondary, currentColor)` for their secondary
 * layer. Override the secondary fill for a subtree by setting
 * `--icon-secondary: var(--accent-soft)` on a parent element.
 */
export function Icon({
  name,
  variant = 'line',
  size = 16,
  className,
  label,
  style,
}: IconProps) {
  const Component = iconRegistry[resolveName(name, variant)];
  const decorative = !label;

  return (
    <Component
      role={decorative ? undefined : 'img'}
      aria-label={label}
      aria-hidden={decorative || undefined}
      focusable={false}
      width={size}
      height={size}
      className={className}
      style={{
        '--icon-secondary': 'currentColor',
        display: 'inline-flex',
        flexShrink: 0,
        color: 'currentColor',
        ...style,
      } as CSSProperties}
    />
  );
}
