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
				colorSigma: props.colorSigma,
			}),
		[],
	);

	useEffect(() => {
		(effect.uniforms.get("uBlurRadius") as Uniform).value =
			props.blurRadius ?? 3;
	}, [effect, props.blurRadius]);

	useEffect(() => {
		(effect.uniforms.get("uDepthThreshold") as Uniform).value =
			props.depthThreshold ?? 0.02;
	}, [effect, props.depthThreshold]);

	useEffect(() => {
		(effect.uniforms.get("uColorSigma") as Uniform).value =
			props.colorSigma ?? 0.25;
	}, [effect, props.colorSigma]);

	return <primitive object={effect} />;
}
