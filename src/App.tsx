import "./App.css";

function App() {
	return (
		<div className="w-screen h-screen bg-background" data-theme="sunset">
			{/* Custom Titlebar */}
			<header className="titlebar">
				<div className="no-drag flex items-center gap-1 ml-auto">
					{/* Your controls can go here */}
				</div>
			</header>

			{/* Main Content with padding-top offset */}
			<main className="pt-titlebar w-full h-full flex border">
				{/* Left Sidebar */}
				<aside className="w-1/4 bg-muted h-full"></aside>

				{/* Main Content Area */}
				<div className="flex-1 flex flex-col h-full">
					{/* Top Half - White */}
					<div className="flex-1"></div>

					{/* Bottom Half - Dark Gray */}
					<div className="flex-1"></div>
				</div>
			</main>
		</div>
	);
}

export default App;
