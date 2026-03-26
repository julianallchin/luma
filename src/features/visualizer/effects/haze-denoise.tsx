import { useEffect, useMemo } from "react";
import type { Uniform } from "three";
import {
	HazeDenoiseEffect,
	type HazeDenoiseOptions,
} from "./haze-denoise-effect";

export function HazeDenoise(props: HazeDenoiseOptions) {
	const effect = useMemo(
		() =>
			new HazeDenoiseEffect({
				blurRadius: props.blurRadius,
				depthThreshold: props.depthThreshold,
			}),
		[],
	);

	useEffect(() => {
		(effect.uniforms.get("uBlurRadius") as Uniform).value =
			props.blurRadius ?? 2;
	}, [effect, props.blurRadius]);

	useEffect(() => {
		(effect.uniforms.get("uDepthThreshold") as Uniform).value =
			props.depthThreshold ?? 0.02;
	}, [effect, props.depthThreshold]);

	return <primitive object={effect} />;
}
