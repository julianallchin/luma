import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";

export type BarClassification = {
	bar_idx: number;
	start: number;
	end: number;
	predictions: Record<string, number>;
};

export type BarClassificationsPayload = {
	classifications: BarClassification[];
	tagOrder: string[];
};

type ServerPayload = {
	classifications: BarClassificationsPayload["classifications"];
	tagOrder: string[];
};

/** Fetches the per-bar classifier output for a track. Null if unavailable. */
export function useBarClassifications(
	trackId: string | null,
): BarClassificationsPayload | null {
	const [data, setData] = useState<BarClassificationsPayload | null>(null);

	useEffect(() => {
		if (!trackId) {
			setData(null);
			return;
		}
		let cancelled = false;
		invoke<ServerPayload | null>("get_track_bar_classifications", { trackId })
			.then((res) => {
				if (cancelled) return;
				if (!res) {
					setData(null);
					return;
				}
				setData({
					classifications: res.classifications,
					tagOrder: res.tagOrder,
				});
			})
			.catch(() => {
				if (!cancelled) setData(null);
			});
		return () => {
			cancelled = true;
		};
	}, [trackId]);

	return data;
}

/** Fetches the bundled per-tag thresholds for the bar-tags debug overlay. */
export function useClassifierThresholds(): Record<string, number> {
	const [thresholds, setThresholds] = useState<Record<string, number>>({});

	useEffect(() => {
		let cancelled = false;
		invoke<Record<string, number>>("get_classifier_thresholds")
			.then((res) => {
				if (!cancelled) setThresholds(res);
			})
			.catch(() => {
				if (!cancelled) setThresholds({});
			});
		return () => {
			cancelled = true;
		};
	}, []);

	return thresholds;
}
