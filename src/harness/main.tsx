import ReactDOM from "react-dom/client";

import "@/App.css";
import { FIXTURES } from "./fixtures";

// Component-screenshot harness page. Served by the Vite dev server at
// /harness.html?id=<fixture-id>. The capture script (harness/shot-web.mjs)
// screenshots the #fixture element once data-ready="1" appears.
declare global {
	interface Window {
		__FIXTURE_IDS__: string[];
	}
}

window.__FIXTURE_IDS__ = FIXTURES.map((f) => f.id);

const id = new URLSearchParams(window.location.search).get("id");
const fixture = FIXTURES.find((f) => f.id === id);

function Harness() {
	if (!fixture) {
		return (
			<div className="p-6 text-foreground text-xs">
				{id ? `Unknown fixture: ${id}` : "Pass ?id=<fixture-id>"}. Available:{" "}
				{window.__FIXTURE_IDS__.join(", ")}
			</div>
		);
	}
	return (
		<div id="fixture" className="inline-block bg-background p-6">
			{fixture.render()}
		</div>
	);
}

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
	<Harness />,
);

document.fonts.ready.then(() => {
	requestAnimationFrame(() =>
		requestAnimationFrame(() => {
			document.getElementById("fixture")?.setAttribute("data-ready", "1");
			document.body.setAttribute("data-ready", "1");
		}),
	);
});
