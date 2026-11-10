import { Heading } from '@astryxdesign/core/Heading';
import { Text } from '@astryxdesign/core/Text';
import { VStack } from '@astryxdesign/core/VStack';
import { FIELDS, type Read } from './types';

/** What was read from a document; choosing a field finds it on the page. */
export function ReadFields({
  read,
  focus,
  onFocus,
}: {
  read: Read;
  focus: string | null;
  onFocus: (key: string | null) => void;
}) {
  return (
    <VStack gap={2}>
      <Heading level={2}>Read from the document</Heading>
      <Text type="supporting">Choose a field to find it on the page.</Text>
      <VStack gap={1}>
        {FIELDS.filter((f) => read[f.key]).map((f) => (
          <button
            key={f.key}
            type="button"
            className={focus === f.key ? 'payvy-field payvy-field-on' : 'payvy-field'}
            onClick={() => onFocus(focus === f.key ? null : f.key)}
          >
            <Text type="supporting">{f.label}</Text>
            <Text hasTabularNumbers>{read[f.key]}</Text>
          </button>
        ))}
      </VStack>
    </VStack>
  );
}
