/** View density for the tracks list: `list` shows cover art, `compact` doesn't. */
export type TracksView = "list" | "compact";

export type Translator = (
  key: string,
  options?: Record<string, unknown>,
) => string;
