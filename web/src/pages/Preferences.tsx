// Your own preferences: how Payvy looks on this device.
import { AppearanceControl } from '../Appearance';
import { Page } from '../Page';
import { SettingsGroup, SettingsRow } from '../SettingsRows';

export function Preferences() {
  return (
    <Page title="Preferences" narrow>
      <SettingsGroup title="Interface and theme">
        <SettingsRow label="Appearance" description="Light, dark, or follow this device. Saved in this browser.">
          <AppearanceControl />
        </SettingsRow>
      </SettingsGroup>
    </Page>
  );
}
