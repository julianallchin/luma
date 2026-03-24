import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

// Suppress unhandled rejections from Tauri event listener cleanup races.
// The primary fix is the init script in lib.rs that wraps runCallback,
// but async unlisten races can still surface as rejected promises.
window.addEventListener("unhandledrejection", (event) => {
	console.error("[unhandledrejection]", event.reason);
	event.preventDefault();
});

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
	<React.StrictMode>
		<App />
	</React.StrictMode>,
);
