/* global document */
// The splash loads none of the bundle, so `lib/nativeContextMenu` is not here:
// it has no text field, and the webview's own menu has no place on it either.
document.addEventListener("contextmenu", (event) => event.preventDefault());
