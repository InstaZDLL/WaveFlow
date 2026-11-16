import type { TFunction } from "i18next";

/** The `t` that `useTranslation()` returns, as the components and helpers
 *  it is handed down to take it. One alias for every file rather than a
 *  `(key, options?) => string` written out in each: i18next types `t` as
 *  one call shape per way of calling it (key alone, key and options, key
 *  and default value…), and a hand-written signature with an optional
 *  second argument matches none of them, so the caller's `t` could no
 *  longer be passed to it. */
export type Translator = TFunction;
