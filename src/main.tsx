import * as Sentry from "@sentry/react";
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";

if (import.meta.env.PROD) {
	Sentry.init({
		dsn: "https://01abb3c36939abaf0327f3117d387f98@o4511152136257536.ingest.us.sentry.io/4511152144711680",
		sendDefaultPii: false,
	});
}

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
