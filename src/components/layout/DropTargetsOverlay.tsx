import type { ReactNode, RefObject } from "react";
import { createPortal } from "react-dom";
import { useTranslation } from "react-i18next";
import { FolderPlus, Play } from "lucide-react";
import type {
  DropTarget,
  DropTargetElements,
} from "../../hooks/useDragDropImport";

/**
 * What a drop on the window can do, shown while files are dragged over
 * it: play them now without importing them, or add them to the library.
 * The pointer picks the target (`useDragDropImport` follows it across the
 * boxes registered in `targetsRef`); the one under it lights up.
 *
 * Portalled to `document.body`, above every skin's stacking contexts.
 * Pointer-events are off so the drop still reaches Tauri's native handler
 * underneath. It goes away on the drop: an import's scan reports in the
 * task status bar like any other, where it can be followed and stopped,
 * rather than behind a screen that hides the app for as long as it runs.
 *
 * Base styling works in every skin; the fluid layout recomposes it through
 * the `data-drop-*` landmarks.
 */
export function DropTargetsOverlay({
  hoveredTarget,
  targetsRef,
}: {
  hoveredTarget: DropTarget | null;
  targetsRef: RefObject<DropTargetElements>;
}) {
  const { t } = useTranslation();

  return createPortal(
    <div
      data-drop-overlay
      className="fixed inset-0 z-100 pointer-events-none flex gap-4 p-6 bg-black/45 backdrop-blur-md animate-fade-in"
    >
      <Target
        target="play"
        active={hoveredTarget === "play"}
        icon={<Play size={30} />}
        title={t("dragDrop.play.title")}
        hint={t("dragDrop.play.hint")}
        targetsRef={targetsRef}
      />
      <Target
        target="import"
        active={hoveredTarget === "import"}
        icon={<FolderPlus size={30} />}
        title={t("dragDrop.import.title")}
        hint={t("dragDrop.import.hint")}
        targetsRef={targetsRef}
      />
    </div>,
    document.body,
  );
}

function Target({
  target,
  active,
  icon,
  title,
  hint,
  targetsRef,
}: {
  target: DropTarget;
  active: boolean;
  icon: ReactNode;
  title: string;
  hint: string;
  targetsRef: RefObject<DropTargetElements>;
}) {
  return (
    <div
      ref={(el) => {
        targetsRef.current[target] = el;
      }}
      data-drop-target={target}
      data-active={active || undefined}
      className="relative flex-1 flex flex-col items-center justify-center gap-3 rounded-2xl bg-white dark:bg-zinc-900 px-8 text-center shadow-2xl"
    >
      {/* The outline is its own layer: a skin that restyles the card as
          one of its panels (border and all) still shows which target is
          the one under the pointer. */}
      <div
        aria-hidden
        data-drop-ring
        className={`pointer-events-none absolute inset-0 rounded-[inherit] border-2 border-dashed transition-colors ${
          active ? "border-emerald-500" : "border-zinc-300 dark:border-zinc-700"
        }`}
      />
      <div
        data-drop-icon
        className={
          active ? "text-emerald-500" : "text-zinc-400 dark:text-zinc-500"
        }
      >
        {icon}
      </div>
      <div
        data-drop-title
        className="text-xl font-semibold text-zinc-900 dark:text-white"
      >
        {title}
      </div>
      <div
        data-drop-hint
        className="max-w-xs text-sm text-zinc-500 dark:text-zinc-400"
      >
        {hint}
      </div>
    </div>
  );
}
