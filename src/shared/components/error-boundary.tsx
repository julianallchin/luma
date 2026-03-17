import { Component, type ErrorInfo, type ReactNode } from "react";

type Props = {
	children: ReactNode;
};

type State = {
	error: Error | null;
};

export class ErrorBoundary extends Component<Props, State> {
	state: State = { error: null };

	static getDerivedStateFromError(error: Error): State {
		return { error };
	}

	componentDidCatch(error: Error, info: ErrorInfo) {
		console.error("[ErrorBoundary] Caught:", error, info.componentStack);
	}

	render() {
		if (this.state.error) {
			return (
				<div className="w-screen h-screen bg-background flex flex-col items-center justify-center gap-4 text-foreground p-8">
					<h1 className="text-lg font-semibold">Something went wrong</h1>
					<pre className="text-xs text-muted-foreground max-w-xl overflow-auto whitespace-pre-wrap bg-muted/30 p-4 rounded">
						{this.state.error.message}
					</pre>
					<button
						type="button"
						onClick={() => this.setState({ error: null })}
						className="text-sm px-4 py-2 bg-foreground text-background rounded hover:opacity-90 transition-opacity"
					>
						Try Again
					</button>
				</div>
			);
		}

		return this.props.children;
	}
}
