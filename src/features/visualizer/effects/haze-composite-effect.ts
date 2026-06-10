import { BlendFunction, Effect, EffectAttribute } from "postprocessing";
import { type Texture, Uniform, Vector2 } from "three";

const fragmentShader = /* glsl */ `
uniform sampler2D uHazeTexture;
uniform vec2 uHazeResolution;   // low-res haze buffer dimensions (px)
uniform float uDepthSigma;      // bilateral depth falloff, in raw-depth units

// Depth-aware (bilateral) upsample of the (half-res) haze. Each of the 4
// nearest low-res taps is weighted by its bilinear position AND by how close
// the scene depth it saw (stored in alpha) is to this full-res pixel's depth.
// That keeps haze from bleeding across geometry silhouettes the way a plain
// bilinear upsample would — beams stay crisply occluded at edges.
vec3 upsampleHaze(vec2 uv, float fullDepth) {
  vec2 R = uHazeResolution;
  vec2 lr = uv * R - 0.5;
  vec2 base = floor(lr);
  vec2 f = lr - base;

  // Sample at exact texel centers so LinearFilter returns un-blended depths.
  vec2 uv00 = (base + vec2(0.5, 0.5)) / R;
  vec2 uv10 = (base + vec2(1.5, 0.5)) / R;
  vec2 uv01 = (base + vec2(0.5, 1.5)) / R;
  vec2 uv11 = (base + vec2(1.5, 1.5)) / R;

  vec4 s00 = texture2D(uHazeTexture, uv00);
  vec4 s10 = texture2D(uHazeTexture, uv10);
  vec4 s01 = texture2D(uHazeTexture, uv01);
  vec4 s11 = texture2D(uHazeTexture, uv11);

  float w00 = (1.0 - f.x) * (1.0 - f.y);
  float w10 = f.x * (1.0 - f.y);
  float w01 = (1.0 - f.x) * f.y;
  float w11 = f.x * f.y;

  // Bilateral depth term — alpha holds the raw scene depth each haze texel saw.
  float k = 1.0 / max(uDepthSigma, 1e-5);
  w00 *= exp(-abs(s00.a - fullDepth) * k);
  w10 *= exp(-abs(s10.a - fullDepth) * k);
  w01 *= exp(-abs(s01.a - fullDepth) * k);
  w11 *= exp(-abs(s11.a - fullDepth) * k);

  vec3 haze = w00 * s00.rgb + w10 * s10.rgb + w01 * s01.rgb + w11 * s11.rgb;
  float wsum = w00 + w10 + w01 + w11;
  return haze / max(wsum, 1e-4);
}

void mainImage(const in vec4 inputColor, const in vec2 uv, const in float depth, out vec4 outputColor) {
  vec3 haze = upsampleHaze(uv, depth);
  outputColor = vec4(inputColor.rgb + haze, inputColor.a);
}
`;

export class HazeCompositeEffect extends Effect {
	private _resolutionScale: number;

	constructor(hazeTexture: Texture | null = null, resolutionScale = 1.0) {
		super("HazeCompositeEffect", fragmentShader, {
			blendFunction: BlendFunction.SRC,
			// Pull full-res scene depth so the upsample can reject taps that sit
			// on a different surface than the pixel being shaded.
			attributes: EffectAttribute.DEPTH,
			uniforms: new Map<string, Uniform>([
				["uHazeTexture", new Uniform(hazeTexture)],
				["uHazeResolution", new Uniform(new Vector2(1, 1))],
				["uDepthSigma", new Uniform(0.005)],
			]),
		});
		this._resolutionScale = resolutionScale;
	}

	setHazeTexture(texture: Texture) {
		(this.uniforms.get("uHazeTexture") as Uniform).value = texture;
	}

	override setSize(width: number, height: number): void {
		// Mirror VolumetricHazePass.setSize so the low-res grid lines up exactly.
		const w = Math.max(1, Math.round(width * this._resolutionScale));
		const h = Math.max(1, Math.round(height * this._resolutionScale));
		(this.uniforms.get("uHazeResolution") as Uniform).value.set(w, h);
	}
}
