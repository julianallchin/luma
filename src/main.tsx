import * as Sentry from "@sentry/react";
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
// Side-effect import: registers the global session-finished toast subscriber.
import "./features/track-editor/agent/auto-light";
import { publishAgentSettings } from "./features/track-editor/agent/openrouter-key";
import { installRenderTelemetryGlobalHandlers } from "./features/visualizer/lib/render-telemetry";

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

installRenderTelemetryGlobalHandlers();

// The agent's provider and key live in the settings table, which the GPUI
// window reads too; this webview's localStorage is a cache of it. Republish at
// boot so a key typed into an older build still reaches the other host.
publishAgentSettings();

// Perf-baseline capture (docs/specs/perf-baseline.md). Off unless
// `localStorage.setItem("luma:perf-baseline", "1")` — one localStorage read
// here is the entire cost when disabled; the module is a separate chunk that
// is never fetched otherwise. The key is written literally (not imported as
// `PERF_BASELINE_FLAG`) precisely so the import stays dynamic.
try {
	if (localStorage.getItem("luma:perf-baseline") === "1") {
		import("./shared/lib/perf-baseline").then((m) => m.installPerfBaseline());
	}
} catch {
	// localStorage unavailable — capture stays off.
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
	<React.StrictMode>
		<App />
	</React.StrictMode>,
);
