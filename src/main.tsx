import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import { MiniPlayerApp } from "./MiniPlayerApp";
import "./app.css";
import "./i18n";

// The mini-player runs in a second WebviewWindow that loads the same
// bundle with `?mini=1` in the URL. We branch here so it boots into
// a stripped-down provider tree (no LibraryContext / sidebar / etc).
const isMini = new URLSearchParams(window.location.search).get("mini") === "1";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>{isMini ? <MiniPlayerApp /> : <App />}</React.StrictMode>,
);
