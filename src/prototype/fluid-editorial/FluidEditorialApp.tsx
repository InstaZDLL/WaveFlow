import { useEffect, useState, type CSSProperties } from "react";
import { useTranslation } from "react-i18next";
// DM Sans upright is bundled by `main.tsx` for the Liquid skin; the
// italic axis is loaded here only, for the prototype's emphasis.
import "@fontsource-variable/dm-sans/opsz-italic.css";
import "./fluid-editorial.css";
import { FluidHome } from "./home/FluidHome";
import { Atmosphere } from "./shell/Atmosphere";
import { FePlayerBar } from "./shell/FePlayerBar";
import { FeSidebar } from "./shell/FeSidebar";
import { FeTopBar } from "./shell/FeTopBar";
import { FeStateProvider } from "./state";
import { useFe } from "./useFe";

/**
 * Fluid Editorial — an isolated Home prototype.
 *
 * Mounted by `main.tsx` when the URL carries `?proto=fluid-editorial`,
 * in development builds only. It renders the whole desktop shell
 * (sidebar, top bar, content column, player bar) on mock state so the
 * direction can be judged as a screen, not a component — while the
 * production `AppLayout`, `HomeView` and every provider stay untouched.
 */
export default function FluidEditorialApp() {
  const { i18n } = useTranslation();
  useEffect(() => {
    const previous = document.title;
    document.title = "WaveFlow — Fluid Editorial (prototype)";
    return () => {
      document.title = previous;
    };
  }, []);
  return (
    <FeStateProvider language={i18n.resolvedLanguage ?? i18n.language}>
      <Shell />
    </FeStateProvider>
  );
}

function Shell() {
  const { atmosphere } = useFe();
  const [collapsed, setCollapsed] = useState(false);
  const vars = {
    ["--fe-accent" as string]: atmosphere.accent,
    ["--fe-accent-ink" as string]: atmosphere.accentInk,
    ["--fe-glow" as string]: atmosphere.glow,
    ["--fe-glow-deep" as string]: atmosphere.glowDeep,
    transition: "--fe-accent 600ms, --fe-glow 600ms",
  } as CSSProperties;

  return (
    <div className="fe flex h-screen flex-col overflow-hidden" style={vars}>
      <div className="flex min-h-0 flex-1">
        <FeSidebar collapsed={collapsed} />
        <div className="relative flex min-w-0 flex-1 flex-col">
          <Atmosphere />
          <FeTopBar
            collapsed={collapsed}
            onToggleSidebar={() => setCollapsed((c) => !c)}
          />
          <main className="relative flex-1 overflow-y-auto px-8 pb-8 pt-3 xl:px-12">
            <FluidHome />
          </main>
        </div>
      </div>
      <FePlayerBar />
    </div>
  );
}
