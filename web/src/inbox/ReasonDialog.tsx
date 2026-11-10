import { useState } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { Dialog, DialogHeader } from '@astryxdesign/core/Dialog';
import { HStack } from '@astryxdesign/core/HStack';
import { TextArea } from '@astryxdesign/core/TextArea';
import { VStack } from '@astryxdesign/core/VStack';

/** Asks why, for rejecting or declining. The reason goes on the record. */
export function ReasonDialog({
  title,
  subtitle,
  actionLabel,
  isRequired,
  onSubmit,
  onClose,
}: {
  title: string;
  subtitle?: string;
  actionLabel: string;
  isRequired: boolean;
  onSubmit: (reason: string) => Promise<void>;
  onClose: () => void;
}) {
  const [reason, setReason] = useState('');
  const [error, setError] = useState<string | null>(null);

  async function submit() {
    setError(null);
    try {
      await onSubmit(reason.trim());
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }

  return (
    <Dialog isOpen onOpenChange={(open) => !open && onClose()} width={460} purpose="form">
      <DialogHeader title={title} subtitle={subtitle} onOpenChange={(open) => !open && onClose()} />
      <VStack gap={4} padding={4}>
        <TextArea
          label="Why"
          value={reason}
          onChange={setReason}
          isRequired={isRequired}
          isOptional={!isRequired}
          description="Kept on the bill's record."
          hasAutoFocus
        />
        {error && <Banner status="error" title={error} />}
        <HStack gap={2} hAlign="end">
          <Button label="Cancel" variant="ghost" onClick={onClose} />
          <Button
            label={actionLabel}
            variant="destructive"
            clickAction={submit}
            isDisabled={isRequired && reason.trim() === ''}
          />
        </HStack>
      </VStack>
    </Dialog>
  );
}
