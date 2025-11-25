export type RenderMetrics = {
	drawFps: number;
	rafFps: number;
	rafDelta: number;
	blockedAvg: number;
	blockedPeak: number;
	totalMs: number;
	sections: {
		ruler: number;
		waveform: number;
		annotations: number;
		minimap: number;
	};
	frame: number;
	avg: {
		ruler: number;
		waveform: number;
		annotations: number;
		minimap: number;
		totalMs: number;
	};
	peak: {
		ruler: number;
		waveform: number;
		annotations: number;
		minimap: number;
		totalMs: number;
	};
};
