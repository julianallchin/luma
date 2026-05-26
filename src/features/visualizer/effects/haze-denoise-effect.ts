import { BlendFunction, Effect, EffectAttribute } from "postprocessing";
import { Uniform } from "three";

const fragmentShader = /* glsl */ `
uniform float uBlurRadius;
uniform float uDepthThreshold;
uniform float uColorSigma;

// À-trous bilateral: 3x3 inner ring + 4-tap outer cross at 2x radius.
// Depth weight preserves geometry edges; color weight preserves the
// hard cone boundaries of volumetric spotlights so they don't smear.
const vec2 kHazeDenoiseOffsets[12] = vec2[12](
  vec2(-1.0, -1.0), vec2( 0.0, -1.0), vec2( 1.0, -1.0),
  vec2(-1.0,  0.0),                   vec2( 1.0,  0.0),
  vec2(-1.0,  1.0), vec2( 0.0,  1.0), vec2( 1.0,  1.0),
  vec2(-2.0,  0.0), vec2( 2.0,  0.0),
  vec2( 0.0, -2.0), vec2( 0.0,  2.0)
);

void mainImage(const in vec4 inputColor, const in vec2 uv, out vec4 outputColor) {
  if (uBlurRadius < 0.001) {
    outputColor = inputColor;
    return;
  }

  float centerDepth = readDepth(uv);
  vec4 centerColor = texture2D(inputBuffer, uv);
  vec3 centerRgb = centerColor.rgb;

  vec2 texelSize = 1.0 / vec2(textureSize(inputBuffer, 0));
  float r = uBlurRadius;
  float invColorSigma2 = 1.0 / max(uColorSigma * uColorSigma, 1e-4);

  vec4 accum = centerColor * 2.0;
  float total = 2.0;

  for (int i = 0; i < 12; i++) {
    vec2 off = kHazeDenoiseOffsets[i] * r * texelSize;
    vec2 sampleUv = uv + off;

    vec4 sampleColor = texture2D(inputBuffer, sampleUv);
    float sampleDepth = readDepth(sampleUv);

    float depthW = step(abs(sampleDepth - centerDepth), uDepthThreshold);
    vec3 dc = sampleColor.rgb - centerRgb;
    float colorW = exp(-dot(dc, dc) * invColorSigma2);

    float w = depthW * colorW;
    accum += sampleColor * w;
    total += w;
  }

  outputColor = accum / total;
}
`;

export interface HazeDenoiseOptions {
	blurRadius?: number;
	depthThreshold?: number;
	/** Color-similarity sigma for the bilateral term. Higher = more smoothing
	 *  across color differences (will smear cone edges). */
	colorSigma?: number;
}

export class HazeDenoiseEffect extends Effect {
	constructor(options: HazeDenoiseOptions = {}) {
		super("HazeDenoiseEffect", fragmentShader, {
			blendFunction: BlendFunction.SKIP,
			attributes: EffectAttribute.DEPTH,
			uniforms: new Map<string, Uniform>([
				["uBlurRadius", new Uniform(options.blurRadius ?? 3)],
				["uDepthThreshold", new Uniform(options.depthThreshold ?? 0.02)],
				["uColorSigma", new Uniform(options.colorSigma ?? 0.25)],
			]),
		});
	}
}
