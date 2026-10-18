import type { ComponentType } from "react";
import {
  CalendarDays,
  Clock,
  Disc,
  Folder,
  Heart,
  Home,
  Mic2,
  Music2,
  Plus,
  Radio,
  Tags,
  type LucideProps,
} from "lucide-react";
import { WaveFlowLogo } from "../../../components/common/WaveFlowLogo";
import { PlaylistIcon } from "../../../lib/PlaylistIcon";
import { resolvePlaylistColor } from "../../../lib/playlistVisuals";
import { fixtures } from "../fixtures";
import { fill } from "../copy";
import { integer } from "../format";
import { useFe } from "../useFe";
import { Mosaic } from "../ui/Cover";

/**
 * Same structure as the production sidebar — brand, profile, pinned
 * nav, "Ma musique", playlists — with the chrome quietened:
 *
 * - no border; the sidebar is a tone, not a box;
 * - section labels in 11 px, medium weight, wide tracking, tertiary ink;
 * - the active state is a 2 px accent bar, not a filled block;
 * - every playlist row carries real artwork (a 2×2 of its covers), so
 *   the column reads as a record shelf rather than a list of icons.
 */
export function FeSidebar({ collapsed }: { collapsed: boolean }) {
  const { copy, lang } = useFe();
  const nav = copy.sidebar;
  const stats = fixtures.stats;
  const user = fixtures.playlists.filter((p) => !p.isSmart);

  return (
    <aside
      className={`flex h-full flex-col ${collapsed ? "w-20" : "w-64"}`}
      style={{ backgroundColor: "var(--fe-surface)" }}
    >
      <div className={`${collapsed ? "px-3 pt-5" : "px-5 pt-5"} pb-3`}>
        <div
          className={`flex items-center ${collapsed ? "justify-center" : "gap-2.5"}`}
        >
          <WaveFlowLogo className="h-6 w-6 shrink-0" />
          {!collapsed && (
            <span className="fe-title text-[1.0625rem] text-[var(--fe-ink)]">
              WaveFlow
            </span>
          )}
        </div>
        {/* Profile: a quiet row, the avatar carrying the accent. */}
        <button
          type="button"
          className={`mt-5 flex w-full items-center rounded-xl py-1.5 ${collapsed ? "justify-center" : "gap-3 px-1"} hover:bg-white/[0.04]`}
          aria-label={copy.topbar.profile}
        >
          <span
            className="grid h-7 w-7 shrink-0 place-items-center rounded-full text-[0.75rem] font-semibold"
            style={{
              backgroundColor: "var(--fe-accent)",
              color: "var(--fe-accent-ink)",
            }}
          >
            P
          </span>
          {!collapsed && (
            <span className="min-w-0 text-left">
              <span className="block truncate text-[0.8125rem] font-medium text-[var(--fe-ink)]">
                Probu
              </span>
              <span className="fe-ink-3 fe-mono block text-[0.6875rem]">
                {integer(stats.tracks, lang)} {copy.stats.tracks}
              </span>
            </span>
          )}
        </button>
      </div>

      <nav
        className={`${collapsed ? "px-3" : "px-4"} space-y-0.5`}
        aria-label="Navigation"
      >
        <NavItem icon={Home} label={nav.home} active collapsed={collapsed} />
        <NavItem icon={Radio} label={nav.webRadio} collapsed={collapsed} />
      </nav>

      <div
        className={`mt-4 min-h-0 flex-1 overflow-y-auto ${collapsed ? "px-3" : "px-4"} pb-4`}
      >
        <SectionLabel collapsed={collapsed}>{nav.myMusic}</SectionLabel>
        <div className="space-y-0.5">
          <NavItem
            icon={Music2}
            label={nav.tracks}
            collapsed={collapsed}
            count={stats.tracks}
            lang={lang}
          />
          <NavItem
            icon={Disc}
            label={nav.albums}
            collapsed={collapsed}
            count={stats.albums}
            lang={lang}
          />
          <NavItem
            icon={Mic2}
            label={nav.artists}
            collapsed={collapsed}
            count={stats.artists}
            lang={lang}
          />
          <NavItem icon={Tags} label={nav.genres} collapsed={collapsed} />
          <NavItem
            icon={CalendarDays}
            label={nav.years}
            collapsed={collapsed}
          />
          <NavItem icon={Folder} label={nav.folders} collapsed={collapsed} />
        </div>

        <div className="mt-5 flex items-center justify-between">
          <SectionLabel collapsed={collapsed}>{nav.playlists}</SectionLabel>
          {!collapsed && (
            <button
              type="button"
              className="fe-icon-btn -mr-1.5 h-7 w-7"
              aria-label={nav.newPlaylist}
              title={nav.newPlaylist}
            >
              <Plus size={14} strokeWidth={1.75} />
            </button>
          )}
        </div>
        <div className="space-y-0.5">
          <ShelfRow
            collapsed={collapsed}
            label={nav.liked}
            sub={fill(nav.trackCount, { count: integer(stats.liked, lang) })}
            tile={
              <span className="grid h-8 w-8 place-items-center rounded-lg bg-white/[0.06] text-[var(--fe-ink-2)]">
                <Heart size={14} strokeWidth={1.75} />
              </span>
            }
          />
          <ShelfRow
            collapsed={collapsed}
            label={nav.recent}
            sub={fill(nav.trackCount, { count: integer(stats.plays, lang) })}
            tile={
              <span className="grid h-8 w-8 place-items-center rounded-lg bg-white/[0.06] text-[var(--fe-ink-2)]">
                <Clock size={14} strokeWidth={1.75} />
              </span>
            }
          />
          {user.map((p) => {
            const color = resolvePlaylistColor(p.colorId);
            return (
              <ShelfRow
                key={p.id}
                collapsed={collapsed}
                label={p.name}
                sub={fill(nav.trackCount, {
                  count: integer(p.trackCount, lang),
                })}
                tile={
                  p.covers.length >= 2 ? (
                    <Mosaic covers={p.covers} className="h-8 w-8 rounded-lg!" />
                  ) : p.covers.length === 1 ? (
                    <img
                      src={p.covers[0]}
                      alt=""
                      className="h-8 w-8 rounded-lg object-cover"
                    />
                  ) : (
                    <span
                      className={`grid h-8 w-8 place-items-center rounded-lg ${color.tileBg} ${color.tileText}`}
                    >
                      <PlaylistIcon iconId={p.iconId} size={14} />
                    </span>
                  )
                }
              />
            );
          })}
        </div>
      </div>
    </aside>
  );
}

function SectionLabel({
  children,
  collapsed,
}: {
  children: string;
  collapsed: boolean;
}) {
  if (collapsed)
    return <div role="separator" className="mx-2 my-3 h-px bg-white/[0.07]" />;
  return <div className="fe-label mb-1.5 px-3">{children}</div>;
}

function NavItem({
  icon: Icon,
  label,
  active,
  collapsed,
  count,
  lang,
}: {
  icon: ComponentType<LucideProps>;
  label: string;
  active?: boolean;
  collapsed: boolean;
  count?: number;
  lang?: "fr" | "en";
}) {
  const { copy } = useFe();
  return (
    <button
      type="button"
      className={`fe-nav-item ${collapsed ? "justify-center px-0!" : ""}`}
      aria-current={active ? "page" : undefined}
      title={collapsed ? label : active ? undefined : copy.proto.hint}
    >
      <Icon size={17} strokeWidth={1.6} />
      {!collapsed && <span className="min-w-0 flex-1 truncate">{label}</span>}
      {!collapsed && count != null && lang && (
        <span className="fe-ink-3 fe-mono text-[0.6875rem]">
          {integer(count, lang)}
        </span>
      )}
    </button>
  );
}

function ShelfRow({
  tile,
  label,
  sub,
  collapsed,
}: {
  tile: React.ReactNode;
  label: string;
  sub: string;
  collapsed: boolean;
}) {
  const { copy } = useFe();
  return (
    <button
      type="button"
      className={`flex w-full items-center rounded-xl py-1.5 text-left hover:bg-white/[0.045] ${collapsed ? "justify-center px-0" : "gap-3 px-2"}`}
      title={collapsed ? label : copy.proto.hint}
    >
      <span className="shrink-0">{tile}</span>
      {!collapsed && (
        <span className="min-w-0 flex-1">
          <span className="block truncate text-[0.8125rem] font-medium text-[var(--fe-ink)]">
            {label}
          </span>
          <span className="fe-ink-3 fe-mono block truncate text-[0.6875rem]">
            {sub}
          </span>
        </span>
      )}
    </button>
  );
}
