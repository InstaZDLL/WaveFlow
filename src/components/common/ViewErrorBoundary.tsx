import { Component, Fragment, type ErrorInfo, type ReactNode } from "react";
import { useTranslation } from "react-i18next";
import { RotateCw, TriangleAlert } from "lucide-react";
import { EmptyState } from "./EmptyState";
import { LazyRetryGeneration } from "../../lib/preloadable";

interface ViewErrorBoundaryProps {
  /** What is on screen. A new value clears a caught error, so leaving a
   *  failed view (or page, or tab) for another one shows the other one. */
  resetKey: unknown;
  /** `md` for a whole view, `sm` for a page or a tab inside one. */
  size?: "sm" | "md";
  children: ReactNode;
}

interface ViewErrorBoundaryState {
  error: unknown;
  /** Bumped by Retry: the subtree is keyed on it, so it mounts afresh. */
  attempt: number;
  /** The `resetKey` last rendered, to notice when it changes. */
  shownKey: unknown;
  /** Moves on with every Retry and every new `resetKey`; a lazy component
   *  below that failed to load imports again only under a new one. */
  generation: number;
}

// Shared by every boundary, so one mounted later never reuses a generation
// a failed lazy component was already rendered under.
let lastGeneration = 0;
const nextGeneration = () => ++lastGeneration;

/**
 * Keeps a view that fails, while its code loads or while it renders, from
 * taking the rest of the interface down with it. The sidebar, the player
 * bar and playback sit outside it and carry on; the failed area shows a
 * short message and a Retry action.
 *
 * Retry remounts the children under a new key. For a chunk that failed to
 * load that is only half of it: `React.lazy` replays its first rejection
 * forever, which is why the views and pages behind this boundary are built
 * with `retryableLazy` / `preloadable` (`lib/preloadable.tsx`), which
 * import again on the first mount under a new `LazyRetryGeneration`.
 */
export class ViewErrorBoundary extends Component<
  ViewErrorBoundaryProps,
  ViewErrorBoundaryState
> {
  state: ViewErrorBoundaryState = {
    error: null,
    attempt: 0,
    shownKey: this.props.resetKey,
    generation: nextGeneration(),
  };

  static getDerivedStateFromError(
    error: unknown,
  ): Partial<ViewErrorBoundaryState> {
    return { error: error ?? new Error("unknown render error") };
  }

  static getDerivedStateFromProps(
    props: ViewErrorBoundaryProps,
    state: ViewErrorBoundaryState,
  ): Partial<ViewErrorBoundaryState> | null {
    // Moved on to something else: show it, whatever failed before.
    if (props.resetKey !== state.shownKey) {
      return {
        error: null,
        shownKey: props.resetKey,
        generation: nextGeneration(),
      };
    }
    return null;
  }

  componentDidCatch(error: unknown, info: ErrorInfo) {
    console.error("[ViewErrorBoundary]", error, info.componentStack);
  }

  private retry = () => {
    this.setState((s) => ({
      error: null,
      attempt: s.attempt + 1,
      generation: nextGeneration(),
    }));
  };

  render() {
    if (this.state.error !== null) {
      return (
        <ViewErrorFallback
          size={this.props.size ?? "md"}
          onRetry={this.retry}
        />
      );
    }
    return (
      <LazyRetryGeneration value={this.state.generation}>
        <Fragment key={this.state.attempt}>{this.props.children}</Fragment>
      </LazyRetryGeneration>
    );
  }
}

function ViewErrorFallback({
  size,
  onRetry,
}: {
  size: "sm" | "md";
  onRetry: () => void;
}) {
  const { t } = useTranslation();
  return (
    <div role="alert">
      <EmptyState
        icon={<TriangleAlert size={size === "sm" ? 32 : 40} />}
        title={t("viewError.title")}
        description={t("viewError.description")}
        size={size}
        className={size === "sm" ? "py-12" : "py-20"}
      >
        <div className="mt-8 flex justify-center">
          <button
            type="button"
            onClick={onRetry}
            className="bg-emerald-500 hover:bg-emerald-600 text-white px-6 py-3 rounded-xl text-sm font-semibold flex items-center space-x-2 transition-colors shadow-sm"
          >
            <RotateCw size={18} />
            <span>{t("viewError.retry")}</span>
          </button>
        </div>
      </EmptyState>
    </div>
  );
}
