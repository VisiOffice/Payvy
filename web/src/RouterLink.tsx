import type { AnchorHTMLAttributes, Ref } from 'react';
import { Link } from 'react-router';

/** Astryx links take `href`; React Router's take `to`. External links stay plain. */
export function RouterLink({
  href = '',
  ref,
  ...rest
}: AnchorHTMLAttributes<HTMLAnchorElement> & { ref?: Ref<HTMLAnchorElement> }) {
  if (/^[a-z]+:/i.test(href) || href.startsWith('//')) {
    return <a ref={ref} href={href} {...rest} />;
  }
  return <Link ref={ref} to={href} {...rest} />;
}
