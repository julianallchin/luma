import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

// Catch unhandled promise rejections from Tauri event listener race conditions
// (e.g. an event dispatched right as unlisten() removes the handler).
// Without this, the rejection propagates as an uncaught error that can blank the
// WebView since there's no JS context left to catch it.
window.addEventListener("unhandledrejection", (event) => {
	console.error("[unhandledrejection]", event.reason);
	event.preventDefault();
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
	<React.StrictMode>
		<App />
	</React.StrictMode>,
);
