import { Search, X } from "lucide-react";
import { useCallback, useRef, useState } from "react";
import { useTracksStore } from "@/features/tracks/stores/use-tracks-store";
import { Button } from "@/shared/components/ui/button";
import {
	Dialog,
	DialogContent,
	DialogHeader,
	DialogTitle,
} from "@/shared/components/ui/dialog";
import { Input } from "@/shared/components/ui/input";
import { useEngineDjStore } from "../stores/use-engine-dj-store";
import { EngineDjImportProgress } from "./engine-dj-import-progress";
import { EngineDjSidebar } from "./engine-dj-sidebar";
import { EngineDjTrackList } from "./engine-dj-track-list";

interface EngineDjBrowserProps {
	open: boolean;
	onOpenChange: (open: boolean) => void;
}

export function EngineDjBrowser({ open, onOpenChange }: EngineDjBrowserProps) {
	const libraryPath = useEngineDjStore((s) => s.libraryPath);
	const libraryInfo = useEngineDjStore((s) => s.libraryInfo);
	const selectedTrackIds = useEngineDjStore((s) => s.selectedTrackIds);
	const importing = useEngineDjStore((s) => s.importing);
	const error = useEngineDjStore((s) => s.error);
	const openLibrary = useEngineDjStore((s) => s.openLibrary);
	const searchFn = useEngineDjStore((s) => s.search);
	const importSelected = useEngineDjStore((s) => s.importSelected);
	const reset = useEngineDjStore((s) => s.reset);
	const refreshTracks = useTracksStore((s) => s.refresh);
	const searchTimeout = useRef<ReturnType<typeof setTimeout> | null>(null);
	const [searchValue, setSearchValue] = useState("");

	const handleOpen = useCallback(async () => {
		await openLibrary();
	}, [openLibrary]);

	const handleSearch = useCallback(
		(value: string) => {
			setSearchValue(value);
			if (searchTimeout.current) clearTimeout(searchTimeout.current);
			searchTimeout.current = setTimeout(() => {
				searchFn(value);
			}, 300);
		},
		[searchFn],
	);

	const handleImport = useCallback(async () => {
		const imported = await importSelected();
		if (imported.length > 0) {
			await refreshTracks();
		}
	}, [importSelected, refreshTracks]);

	const handleClose = useCallback(
		(nextOpen: boolean) => {
			if (!nextOpen) {
				reset();
				setSearchValue("");
			}
			onOpenChange(nextOpen);
		},
		[onOpenChange, reset],
	);

	return (
		<Dialog open={open} onOpenChange={handleClose}>
			<DialogContent className="max-w-4xl h-[600px] flex flex-col p-0 gap-0">
				<DialogHeader className="px-4 py-3 border-b border-border/50 shrink-0">
					<DialogTitle className="text-sm font-medium">
						{libraryInfo
							? `Engine DJ Library — ${libraryInfo.trackCount} tracks`
							: "Import from Engine DJ"}
					</DialogTitle>
				</DialogHeader>

				{error && (
					<div className="bg-destructive/10 px-4 py-2 text-xs text-destructive border-b border-destructive/20 shrink-0">
						{error}
					</div>
				)}

				{!libraryPath ? (
					<div className="flex-1 flex items-center justify-center">
						<div className="text-center space-y-4">
							<p className="text-sm text-muted-foreground">
								Connect to your Engine DJ library to browse and import
								tracks.
							</p>
							<Button onClick={handleOpen} size="sm">
								Open Engine Library
							</Button>
						</div>
					</div>
				) : (
					<>
						<div className="flex items-center gap-2 px-4 py-2 border-b border-border/50 shrink-0">
							<Search className="size-3.5 text-muted-foreground" />
							<Input
								placeholder="Search tracks..."
								value={searchValue}
								onChange={(e) => handleSearch(e.target.value)}
								className="h-7 text-xs border-0 bg-transparent shadow-none focus-visible:ring-0 p-0"
							/>
							{searchValue && (
								<button
									type="button"
									onClick={() => handleSearch("")}
									className="text-muted-foreground hover:text-foreground"
								>
									<X className="size-3.5" />
								</button>
							)}
						</div>
						<div className="flex flex-1 overflow-hidden">
							<EngineDjSidebar />
							<EngineDjTrackList />
						</div>
						<EngineDjImportProgress />
						<div className="flex items-center justify-between px-4 py-3 border-t border-border/50 shrink-0">
							<div className="text-xs text-muted-foreground">
								{selectedTrackIds.size > 0
									? `${selectedTrackIds.size} tracks selected`
									: "Select tracks to import"}
							</div>
							<Button
								size="sm"
								onClick={handleImport}
								disabled={
									selectedTrackIds.size === 0 || importing
								}
							>
								{importing
									? "Importing..."
									: `Import Selected (${selectedTrackIds.size})`}
							</Button>
						</div>
					</>
				)}
			</DialogContent>
		</Dialog>
	);
}
