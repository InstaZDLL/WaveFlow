import { lazy, useState, type ComponentType } from "react";

/**
 * A lazily loaded component that stops being lazy once its chunk is in.
 *
 * `React.lazy` suspends the first time each component renders, even when
 * its module was fetched long before: the promise it holds only settles a
 * microtask later. Under a `Suspense` boundary already on screen, that
 * swaps the page for the fallback, and React then holds the new content
 * back for its fallback throttle, about 300 ms. Once `preload()` has
 * resolved, this renders the loaded component directly and never suspends;
 * before that, it is `React.lazy`.
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
  const Lazy = lazy(() =>
    preload().then((component) => ({ default: component })),
  );
  function Preloadable(props: P) {
    // Chosen once per mount: swapping `Lazy` for the loaded component on a
    // later render would be a different element type, which remounts
    // everything below it.
    const [Component] = useState<ComponentType<P>>(() => loaded ?? Lazy);
    return <Component {...props} />;
  }
  return Object.assign(Preloadable, { preload });
}
