import { getCurrentWindow } from "@tauri-apps/api/window";

/// Custom minimize / maximize / close buttons. Used in place of native
/// traffic lights because `decorations: false` on the window strips the
/// platform chrome on all OSes.
export function WindowControls() {
	const win = getCurrentWindow();
	return (
		<div className="no-drag flex items-center gap-1">
			<button
				type="button"
				onClick={() => void win.minimize()}
				className="w-5 h-5 flex items-center justify-center text-muted-foreground hover:text-foreground hover:bg-white/5 transition-colors"
				aria-label="Minimize"
			>
				<svg width="8" height="8" viewBox="0 0 10 10" aria-hidden="true">
					<rect x="0" y="4.5" width="10" height="1" fill="currentColor" />
				</svg>
			</button>
			<button
				type="button"
				onClick={() => void win.toggleMaximize()}
				className="w-5 h-5 flex items-center justify-center text-muted-foreground hover:text-foreground hover:bg-white/5 transition-colors"
				aria-label="Maximize"
			>
				<svg width="8" height="8" viewBox="0 0 10 10" aria-hidden="true">
					<rect
						x="0.5"
						y="0.5"
						width="9"
						height="9"
						fill="none"
						stroke="currentColor"
					/>
				</svg>
			</button>
			<button
				type="button"
				onClick={() => void win.close()}
				className="w-5 h-5 flex items-center justify-center text-muted-foreground hover:text-white hover:bg-red-600 transition-colors"
				aria-label="Close"
			>
				<svg width="8" height="8" viewBox="0 0 10 10" aria-hidden="true">
					<path
						d="M1 1 L9 9 M9 1 L1 9"
						stroke="currentColor"
						strokeWidth="1"
						fill="none"
					/>
				</svg>
			</button>
		</div>
	);
}
