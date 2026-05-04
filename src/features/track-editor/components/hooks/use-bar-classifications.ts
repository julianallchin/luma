import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import type { BarClassificationsPayload } from "../../agent/build-context";

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

/** Fetches the bundled per-tag suggestion thresholds (model-tuned). Empty
 * map until loaded; consumers should fall back to a 0.5 default per tag. */
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
