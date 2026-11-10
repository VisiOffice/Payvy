// The frame every list and detail page sits in: a readable width, the title,
// and a line saying what the page is for.
import type { ReactNode } from 'react';
import { Heading } from '@astryxdesign/core/Heading';
import { HStack } from '@astryxdesign/core/HStack';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';

export function Page({
  title,
  description,
  above,
  actions,
  narrow,
  children,
}: {
  title: ReactNode;
  description?: ReactNode;
  /** Breadcrumbs, shown over the title. */
  above?: ReactNode;
  /** Shown beside the title. */
  actions?: ReactNode;
  /** For pages of cards rather than tables. */
  narrow?: boolean;
  children?: ReactNode;
}) {
  return (
    <div className={narrow ? 'payvy-page payvy-page-narrow' : 'payvy-page'}>
      <VStack gap={6}>
        <VStack gap={2}>
          {above}
          <HStack gap={3} hAlign="between" vAlign="center" wrap="wrap">
            {typeof title === 'string' ? <Heading level={1}>{title}</Heading> : title}
            {actions}
          </HStack>
          {description && <Text type="supporting">{description}</Text>}
        </VStack>
        {children}
      </VStack>
    </div>
  );
}
