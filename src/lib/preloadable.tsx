import {
  createContext,
  lazy,
  useContext,
  useState,
  type ComponentType,
} from "react";

/**
 * Which attempt the nearest `ViewErrorBoundary` is on. It moves on when
 * Retry is pressed and when what the boundary wraps changes; a
 * `retryableLazy` whose import failed tries again only once it has.
 */
export const LazyRetryGeneration = createContext(0);

/**
 * `React.lazy` that can load its chunk again after a failure.
 *
 * A lazy component keeps the outcome of its one import for good: once the
 * import rejects, every later render throws the same error, so a retry
 * would replay the failure without fetching anything. This swaps in a
 * fresh lazy component, with a fresh import, for the first mount after a
 * failure under a new `LazyRetryGeneration`. Within one generation the
 * failed one is kept: React renders a mount that never committed more
 * than once, and a fresh import on each of those renders would fetch the
 * chunk again every time.
 */
export function retryableLazy<P extends object>(
  load: () => Promise<ComponentType<P>>,
) {
  let failed = false;
  // The generation the current lazy component was first rendered under.
  let generation: number | undefined;
  const make = () =>
    lazy(() =>
      load().then(
        (component) => ({ default: component }),
        (error: unknown) => {
          failed = true;
          throw error;
        },
      ),
    );
  let Lazy = make();
  function RetryableLazy(props: P) {
    const current = useContext(LazyRetryGeneration);
    // Chosen once per mount, for the reason given in `preloadable` below.
    const [Component] = useState(() => {
      if (failed && current !== generation) {
        failed = false;
        Lazy = make();
        generation = current;
      }
      generation ??= current;
      return Lazy;
    });
    return <Component {...props} />;
  }
  return RetryableLazy;
}

/**
 * A lazily loaded component that stops being lazy once its chunk is in.
 *
 * `React.lazy` suspends the first time each component renders, even when
 * its module was fetched long before: the promise it holds only settles a
 * microtask later. Under a `Suspense` boundary already on screen, that
 * swaps the page for the fallback, and React then holds the new content
 * back for its fallback throttle, about 300 ms. Once `preload()` has
 * resolved, this renders the loaded component directly and never suspends;
 * before that, it is a `retryableLazy`.
 */
export function preloadable<P extends object>(
  load: () => Promise<ComponentType<P>>,
) {
  let loaded: ComponentType<P> | undefined;
  let pending: Promise<ComponentType<P>> | undefined;
  // A failed load is forgotten, so the next open tries again instead of
  // replaying the same rejection for the rest of the session.
  const preload = () =>
    (pending ??= load().then(
      (component) => (loaded = component),
      (error: unknown) => {
        pending = undefined;
        throw error;
      },
    ));
  const Lazy = retryableLazy(preload);
  function Preloadable(props: P) {
    // Chosen once per mount: swapping `Lazy` for the loaded component on a
    // later render would be a different element type, which remounts
    // everything below it.
    const [Component] = useState<ComponentType<P>>(() => loaded ?? Lazy);
    return <Component {...props} />;
  }
  return Object.assign(Preloadable, { preload });
}
