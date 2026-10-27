import { GlobalShortcutsCard } from "../GlobalShortcutsCard";
import { SettingsCategoryHeader } from "../SettingsCategoryHeader";
import { SettingsGroup } from "../SettingsGroup";
import { ShortcutsCard } from "../ShortcutsCard";

interface ShortcutsSectionProps {
  targetGroup: string | null;
}

export function ShortcutsSection({ targetGroup }: ShortcutsSectionProps) {
  return (
    <section
      aria-labelledby="settings-heading-shortcuts"
      className="settings-category"
    >
      <SettingsCategoryHeader category="shortcuts" />
      <SettingsGroup id="keyboard" bare reveal={targetGroup === "keyboard"}>
        <ShortcutsCard />
      </SettingsGroup>
      <SettingsGroup
        id="globalShortcuts"
        bare
        reveal={targetGroup === "globalShortcuts"}
      >
        <GlobalShortcutsCard />
      </SettingsGroup>
    </section>
  );
}
