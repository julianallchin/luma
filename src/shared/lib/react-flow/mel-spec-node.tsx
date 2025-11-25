import * as React from "react";
import type { NodeProps } from "reactflow";
import { BaseNode, usePatternEntryPlayback } from "./base-node";
import type { MelSpecNodeData } from "./types";

export const MAGMA_LUT = [
	[0, 0, 4],
	[1, 0, 5],
	[1, 1, 6],
	[1, 1, 8],
	[2, 1, 9],
	[2, 2, 11],
	[2, 2, 13],
	[3, 3, 15],
	[3, 3, 18],
	[4, 4, 20],
	[5, 4, 22],
	[6, 5, 24],
	[6, 5, 26],
	[7, 6, 28],
	[8, 7, 30],
	[9, 7, 32],
	[10, 8, 34],
	[11, 9, 36],
	[12, 9, 38],
	[13, 10, 41],
	[14, 11, 43],
	[16, 11, 45],
	[17, 12, 47],
	[18, 13, 49],
	[19, 13, 52],
	[20, 14, 54],
	[21, 14, 56],
	[22, 15, 59],
	[24, 15, 61],
	[25, 16, 63],
	[26, 16, 66],
	[28, 16, 68],
	[29, 17, 71],
	[30, 17, 73],
	[32, 17, 75],
	[33, 17, 78],
	[34, 17, 80],
	[36, 18, 83],
	[37, 18, 85],
	[39, 18, 88],
	[41, 17, 90],
	[42, 17, 92],
	[44, 17, 95],
	[45, 17, 97],
	[47, 17, 99],
	[49, 17, 101],
	[51, 16, 103],
	[52, 16, 105],
	[54, 16, 107],
	[56, 16, 108],
	[57, 15, 110],
	[59, 15, 112],
	[61, 15, 113],
	[63, 15, 114],
	[64, 15, 116],
	[66, 15, 117],
	[68, 15, 118],
	[69, 16, 119],
	[71, 16, 120],
	[73, 16, 120],
	[74, 16, 121],
	[76, 17, 122],
	[78, 17, 123],
	[79, 18, 123],
	[81, 18, 124],
	[82, 19, 124],
	[84, 19, 125],
	[86, 20, 125],
	[87, 21, 126],
	[89, 21, 126],
	[90, 22, 126],
	[92, 22, 127],
	[93, 23, 127],
	[95, 24, 127],
	[96, 24, 128],
	[98, 25, 128],
	[100, 26, 128],
	[101, 26, 128],
	[103, 27, 128],
	[104, 28, 129],
	[106, 28, 129],
	[107, 29, 129],
	[109, 29, 129],
	[110, 30, 129],
	[112, 31, 129],
	[114, 31, 129],
	[115, 32, 129],
	[117, 33, 129],
	[118, 33, 129],
	[120, 34, 129],
	[121, 34, 130],
	[123, 35, 130],
	[124, 35, 130],
	[126, 36, 130],
	[128, 37, 130],
	[129, 37, 129],
	[131, 38, 129],
	[132, 38, 129],
	[134, 39, 129],
	[136, 39, 129],
	[137, 40, 129],
	[139, 41, 129],
	[140, 41, 129],
	[142, 42, 129],
	[144, 42, 129],
	[145, 43, 129],
	[147, 43, 128],
	[148, 44, 128],
	[150, 44, 128],
	[152, 45, 128],
	[153, 45, 128],
	[155, 46, 127],
	[156, 46, 127],
	[158, 47, 127],
	[160, 47, 127],
	[161, 48, 126],
	[163, 48, 126],
	[165, 49, 126],
	[166, 49, 125],
	[168, 50, 125],
	[170, 51, 125],
	[171, 51, 124],
	[173, 52, 124],
	[174, 52, 123],
	[176, 53, 123],
	[178, 53, 123],
	[179, 54, 122],
	[181, 54, 122],
	[183, 55, 121],
	[184, 55, 121],
	[186, 56, 120],
	[188, 57, 120],
	[189, 57, 119],
	[191, 58, 119],
	[192, 58, 118],
	[194, 59, 117],
	[196, 60, 117],
	[197, 60, 116],
	[199, 61, 115],
	[200, 62, 115],
	[202, 62, 114],
	[204, 63, 113],
	[205, 64, 113],
	[207, 64, 112],
	[208, 65, 111],
	[210, 66, 111],
	[211, 67, 110],
	[213, 68, 109],
	[214, 69, 108],
	[216, 69, 108],
	[217, 70, 107],
	[219, 71, 106],
	[220, 72, 105],
	[222, 73, 104],
	[223, 74, 104],
	[224, 76, 103],
	[226, 77, 102],
	[227, 78, 101],
	[228, 79, 100],
	[229, 80, 100],
	[231, 82, 99],
	[232, 83, 98],
	[233, 84, 98],
	[234, 86, 97],
	[235, 87, 96],
	[236, 88, 96],
	[237, 90, 95],
	[238, 91, 94],
	[239, 93, 94],
	[240, 95, 94],
	[241, 96, 93],
	[242, 98, 93],
	[242, 100, 92],
	[243, 101, 92],
	[244, 103, 92],
	[244, 105, 92],
	[245, 107, 92],
	[246, 108, 92],
	[246, 110, 92],
	[247, 112, 92],
	[247, 114, 92],
	[248, 116, 92],
	[248, 118, 92],
	[249, 120, 93],
	[249, 121, 93],
	[249, 123, 93],
	[250, 125, 94],
	[250, 127, 94],
	[250, 129, 95],
	[251, 131, 95],
	[251, 133, 96],
	[251, 135, 97],
	[252, 137, 97],
	[252, 138, 98],
	[252, 140, 99],
	[252, 142, 100],
	[252, 144, 101],
	[253, 146, 102],
	[253, 148, 103],
	[253, 150, 104],
	[253, 152, 105],
	[253, 154, 106],
	[253, 155, 107],
	[254, 157, 108],
	[254, 159, 109],
	[254, 161, 110],
	[254, 163, 111],
	[254, 165, 113],
	[254, 167, 114],
	[254, 169, 115],
	[254, 170, 116],
	[254, 172, 118],
	[254, 174, 119],
	[254, 176, 120],
	[254, 178, 122],
	[254, 180, 123],
	[254, 182, 124],
	[254, 183, 126],
	[254, 185, 127],
	[254, 187, 129],
	[254, 189, 130],
	[254, 191, 132],
	[254, 193, 133],
	[254, 194, 135],
	[254, 196, 136],
	[254, 198, 138],
	[254, 200, 140],
	[254, 202, 141],
	[254, 204, 143],
	[254, 205, 144],
	[254, 207, 146],
	[254, 209, 148],
	[254, 211, 149],
	[254, 213, 151],
	[254, 215, 153],
	[254, 216, 154],
	[253, 218, 156],
	[253, 220, 158],
	[253, 222, 160],
	[253, 224, 161],
	[253, 226, 163],
	[253, 227, 165],
	[253, 229, 167],
	[253, 231, 169],
	[253, 233, 170],
	[253, 235, 172],
	[252, 236, 174],
	[252, 238, 176],
	[252, 240, 178],
	[252, 242, 180],
	[252, 244, 182],
	[252, 246, 184],
	[252, 247, 185],
	[252, 249, 187],
	[252, 251, 189],
	[252, 253, 191],
];

function magmaColor(value: number): [number, number, number] {
	const x = Math.min(1, Math.max(0, value));
	const idx = x * (MAGMA_LUT.length - 1);
	const i0 = Math.floor(idx);
	const i1 = Math.min(MAGMA_LUT.length - 1, i0 + 1);
	const f = idx - i0;

	const [r0, g0, b0] = MAGMA_LUT[i0];
	const [r1, g1, b1] = MAGMA_LUT[i1];

	const r = r0 + f * (r1 - r0);
	const g = g0 + f * (g1 - g0);
	const b = b0 + f * (b1 - b0);
	return [r, g, b];
}

export function MelSpecNode(props: NodeProps<MelSpecNodeData>) {
	const { data } = props;
	const canvasRef = React.useRef<HTMLCanvasElement>(null);
	const playback = usePatternEntryPlayback(data.playbackSourceId);

	React.useEffect(() => {
		if (!data.melSpec) return;
		const canvas = canvasRef.current;
		if (!canvas) return;
		const ctx = canvas.getContext("2d");
		if (!ctx) return;

		const { width, height, data: specData, beatGrid } = data.melSpec;
		const aspect = width / Math.max(1, height);
		const MIN_HEIGHT = 160;
		const MAX_HEIGHT = 320;
		let displayHeight = Math.max(MIN_HEIGHT, Math.min(MAX_HEIGHT, height * 2));
		let displayWidth = displayHeight * aspect;
		const MIN_WIDTH = 360;
		const MAX_WIDTH = 720;
		if (displayWidth < MIN_WIDTH) {
			displayWidth = MIN_WIDTH;
			displayHeight = Math.round(displayWidth / aspect);
		}
		if (displayWidth > MAX_WIDTH) {
			displayWidth = MAX_WIDTH;
			displayHeight = Math.round(displayWidth / aspect);
		}
		const dpr = window.devicePixelRatio ?? 1;
		canvas.width = Math.round(displayWidth * dpr);
		canvas.height = Math.round(displayHeight * dpr);
		canvas.style.width = `${displayWidth}px`;
		canvas.style.height = `${displayHeight}px`;

		const offscreen = document.createElement("canvas");
		offscreen.width = width;
		offscreen.height = height;
		const offCtx = offscreen.getContext("2d");
		if (!offCtx) return;

		const imageData = offCtx.createImageData(width, height);
		for (let col = 0; col < width; col += 1) {
			for (let row = 0; row < height; row += 1) {
				const value = specData[col * height + (height - 1 - row)] ?? 0;
				const [rFloat, gFloat, bFloat] = magmaColor(value);
				const r = Math.round(rFloat);
				const g = Math.round(gFloat);
				const b = Math.round(bFloat);
				const index = (row * width + col) * 4;
				imageData.data[index] = r;
				imageData.data[index + 1] = g;
				imageData.data[index + 2] = b;
				imageData.data[index + 3] = 255;
			}
		}
		offCtx.putImageData(imageData, 0, 0);

		ctx.save();
		ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
		ctx.imageSmoothingEnabled = false;
		ctx.clearRect(0, 0, displayWidth, displayHeight);
		ctx.drawImage(offscreen, 0, 0, displayWidth, displayHeight);

		// Draw beat grid lines if available
		if (beatGrid) {
			// Calculate duration from beat grid (use max time from beats and downbeats)
			const allTimes = [...beatGrid.beats, ...beatGrid.downbeats];
			const duration = allTimes.length > 0 ? Math.max(...allTimes) : 0;

			if (duration > 0) {
				const scaleX = displayWidth / duration;

				// Draw beats as thin black lines
				ctx.strokeStyle = "#000000";
				ctx.lineWidth = 0.5;
				for (const beatTime of beatGrid.beats) {
					const x = beatTime * scaleX;
					ctx.beginPath();
					ctx.moveTo(x, 0);
					ctx.lineTo(x, displayHeight);
					ctx.stroke();
				}

				// Draw downbeats as very bright light blue lines
				ctx.strokeStyle = "#52e0ff"; // bright light blue
				ctx.lineWidth = 1;
				for (const downbeatTime of beatGrid.downbeats) {
					const x = downbeatTime * scaleX;
					ctx.beginPath();
					ctx.moveTo(x, 0);
					ctx.lineTo(x, displayHeight);
					ctx.stroke();
				}
			}
		}

		ctx.restore();
	}, [data.melSpec]);

	const melSpecAvailable = Boolean(data.melSpec);

	const handleScrub = React.useCallback(
		(event: React.PointerEvent<HTMLDivElement>) => {
			event.preventDefault();
		},
		[],
	);

	const body = (
		<div className="text-[11px]">
			<div
				className={`relative ${playback.hasActive ? "cursor-pointer" : "cursor-default"}`}
				onPointerDown={handleScrub}
			>
				{melSpecAvailable ? (
					<canvas
						ref={canvasRef}
						className="block bg-black"
						style={{ imageRendering: "pixelated" as const }}
						role="img"
						aria-label="Mel spectrogram"
					/>
				) : (
					<p className="text-muted-foreground">
						Send an audio signal to view its spectrogram.
					</p>
				)}
				{playback.hasActive && (
					<div
						className="pointer-events-none absolute inset-y-0 w-px bg-red-500/80"
						style={{ left: `${playback.progress * 100}%` }}
					/>
				)}
			</div>
		</div>
	);

	return <BaseNode {...props} data={{ ...data, body }} />;
}

