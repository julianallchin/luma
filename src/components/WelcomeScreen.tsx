import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import { useAppViewStore } from "../useAppViewStore";
import { Button } from "./ui/button";

interface RecentProject {
	path: string;
	name: string;
	last_opened: string;
}

export function WelcomeScreen() {
	const [recentProjects, setRecentProjects] = useState<RecentProject[]>([]);
	const setProject = useAppViewStore((state) => state.setProject);

	useEffect(() => {
		invoke<RecentProject[]>("get_recent_projects")
			.then(setRecentProjects)
			.catch(console.error);
	}, []);

	const handleNewProject = async () => {
		try {
			const path = await save({
				filters: [{ name: "Luma Project", extensions: ["luma"] }],
			});
			if (path) {
				await invoke("create_project", { path });
				// Extract filename safely
				const name =
					path.split(/[/\\]/).pop()?.replace(".luma", "") || "Untitled";
				setProject({ path, name });
			}
		} catch (e) {
			console.error("Failed to create project", e);
		}
	};

	const handleOpenProject = async () => {
		try {
			const path = await open({
				filters: [{ name: "Luma Project", extensions: ["luma"] }],
				multiple: false,
			});
			if (path) {
				await invoke("open_project", { path: path as string });
				const name =
					(path as string).split(/[/\\]/).pop()?.replace(".luma", "") ||
					"Untitled";
				setProject({ path: path as string, name });
			}
		} catch (e) {
			console.error("Failed to open project", e);
		}
	};

	const handleOpenRecent = async (path: string) => {
		try {
			await invoke("open_project", { path });
			const name =
				path.split(/[/\\]/).pop()?.replace(".luma", "") || "Untitled";
			setProject({ path, name });
		} catch (e) {
			console.error("Failed to open recent project", e);
		}
	};

	return (
		<div className="relative h-full w-full bg-background text-foreground">
			<div className="absolute top-1/2 left-1/2 -translate-x-1/2 -translate-y-1/2 flex flex-col items-center gap-8">
				<h1 className="text-6xl font-extralight tracking-[0.2em] opacity-80 select-none">
					luma
				</h1>

				<div className="flex flex-col gap-4 w-64 z-10">
					<Button
						onClick={handleNewProject}
						variant="outline"
						className="w-full"
					>
						new project
					</Button>
					<Button
						onClick={handleOpenProject}
						variant="outline"
						className="w-full"
					>
						open project
					</Button>
				</div>

				<div className="absolute top-full left-1/2 -translate-x-1/2 mt-12 w-80">
					{recentProjects.length > 0 && (
						<div className="flex flex-col gap-1 animate-in fade-in duration-500 slide-in-from-top-4">
							{recentProjects.map((p) => (
								<Button
									key={p.path}
									variant="ghost"
									className="justify-start font-light text-sm h-auto py-2 px-4 w-full"
									onClick={() => handleOpenRecent(p.path)}
								>
									<div className="flex flex-col items-start w-full overflow-hidden">
										<span className="text-foreground/80">{p.name}</span>
										<span className="text-xs text-muted-foreground truncate w-full opacity-50">
											{p.path}
										</span>
									</div>
								</Button>
							))}
						</div>
					)}
				</div>
			</div>
		</div>
	);
}
