import { BlendFunction, Effect, EffectAttribute } from "postprocessing";
import { Uniform } from "three";

const fragmentShader = /* glsl */ `
uniform float uBlurRadius;
uniform float uDepthThreshold;

void mainImage(const in vec4 inputColor, const in vec2 uv, out vec4 outputColor) {
  if (uBlurRadius < 0.001) {
    outputColor = inputColor;
    return;
  }

  float centerDepth = readDepth(uv);

  vec2 texelSize = 1.0 / vec2(textureSize(inputBuffer, 0));
  float r = uBlurRadius;

  // 5-tap cross: center + ±1 in X and Y
  vec4 c  = texture2D(inputBuffer, uv);
  vec4 l  = texture2D(inputBuffer, uv + vec2(-r, 0.0) * texelSize);
  vec4 ri = texture2D(inputBuffer, uv + vec2( r, 0.0) * texelSize);
  vec4 u  = texture2D(inputBuffer, uv + vec2(0.0,  r) * texelSize);
  vec4 d  = texture2D(inputBuffer, uv + vec2(0.0, -r) * texelSize);

  float dl = abs(readDepth(uv + vec2(-r, 0.0) * texelSize) - centerDepth);
  float dr = abs(readDepth(uv + vec2( r, 0.0) * texelSize) - centerDepth);
  float du = abs(readDepth(uv + vec2(0.0,  r) * texelSize) - centerDepth);
  float dd = abs(readDepth(uv + vec2(0.0, -r) * texelSize) - centerDepth);

  float wl = step(dl, uDepthThreshold);
  float wr = step(dr, uDepthThreshold);
  float wu = step(du, uDepthThreshold);
  float wd = step(dd, uDepthThreshold);

  float total = 2.0 + wl + wr + wu + wd;
  vec4 result = c * 2.0 + l * wl + ri * wr + u * wu + d * wd;

  outputColor = result / total;
}
`;

export interface HazeDenoiseOptions {
	blurRadius?: number;
	depthThreshold?: number;
}

export class HazeDenoiseEffect extends Effect {
	constructor(options: HazeDenoiseOptions = {}) {
		super("HazeDenoiseEffect", fragmentShader, {
			blendFunction: BlendFunction.SKIP,
			attributes: EffectAttribute.DEPTH,
			uniforms: new Map<string, Uniform>([
				["uBlurRadius", new Uniform(options.blurRadius ?? 2)],
				["uDepthThreshold", new Uniform(options.depthThreshold ?? 0.02)],
			]),
		});
	}
}
