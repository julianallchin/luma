import { Pass } from "postprocessing";
import {
	HalfFloatType,
	LinearFilter,
	RGBAFormat,
	ShaderMaterial,
	type Texture,
	Uniform,
	type WebGLRenderer,
	WebGLRenderTarget,
} from "three";

const vertexShader = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = vec4(position.xy, 1.0, 1.0);
}
`;

// Exponential moving average of the haze buffer across frames. Paired with the
// per-frame jittered ray start in the raymarch, a low-step march converges to
// a clean high-step look over ~1/uAlpha frames. Replaces the spatial Gaussian,
// which smeared beam edges.
const fragmentShader = /* glsl */ `
varying vec2 vUv;

uniform sampler2D uInput;    // this frame's freshly marched haze (rgb=scatter, a=depth)
uniform sampler2D uHistory;  // last frame's accumulated result
uniform float uAlpha;        // base blend weight on stable pixels (~0.1)
uniform float uSnapLo;       // relative luma change where snapping starts
uniform float uSnapHi;       // relative luma change that fully snaps (alpha→1)
uniform float uLumaEps;      // guards the relative-change denominator near black

void main() {
  vec4 cur  = texture2D(uInput, vUv);
  vec3 hist = texture2D(uHistory, vUv).rgb;

  // Content-adaptive blend. Average over many frames to kill dither on stable
  // pixels, but snap toward the current frame when luminance changes fast — a
  // strobe (or a beam edge sweeping past) goes black<->white in ~1 frame with
  // no trailing. Relative change so the gate is scale-independent across HDR.
  float lc = dot(cur.rgb, vec3(0.2126, 0.7152, 0.0722));
  float lh = dot(hist,    vec3(0.2126, 0.7152, 0.0722));
  float change = abs(lc - lh) / (max(lc, lh) + uLumaEps);
  float a = mix(uAlpha, 1.0, smoothstep(uSnapLo, uSnapHi, change));

  // Pass the *current* depth straight through (geometric, not averaged).
  gl_FragColor = vec4(mix(hist, cur.rgb, a), cur.a);
}
`;

export interface HazeTemporalPassOptions {
	/** Base blend weight on stable pixels; ~0.1 ≈ 10-frame average. Default 0.1. */
	alpha?: number;
	/** Relative luma change where snapping begins. Default 0.25. */
	snapLo?: number;
	/** Relative luma change that fully snaps to the current frame. Default 0.8. */
	snapHi?: number;
	/** Floor for the relative-change denominator, avoids snapping in the dark. Default 0.5. */
	lumaEps?: number;
	/** RT resolution scale, should match the haze RT. Default 1.0. */
	resolutionScale?: number;
}

export class HazeTemporalPass extends Pass {
	private _material: ShaderMaterial;
	private _pingRT: WebGLRenderTarget;
	private _pongRT: WebGLRenderTarget;
	private _resolutionScale: number;
	private _outputConsumer: ((tex: Texture) => void) | null = null;

	constructor(options: HazeTemporalPassOptions = {}) {
		super("HazeTemporalPass");
		this.needsSwap = false;
		this.needsDepthTexture = false;

		this._resolutionScale = options.resolutionScale ?? 1.0;

		this._material = new ShaderMaterial({
			name: "HazeTemporalMaterial",
			vertexShader,
			fragmentShader,
			depthTest: false,
			depthWrite: false,
			uniforms: {
				uInput: new Uniform(null),
				uHistory: new Uniform(null),
				uAlpha: new Uniform(options.alpha ?? 0.1),
				uSnapLo: new Uniform(options.snapLo ?? 0.25),
				uSnapHi: new Uniform(options.snapHi ?? 0.8),
				uLumaEps: new Uniform(options.lumaEps ?? 0.5),
			},
		});
		this.fullscreenMaterial = this._material;

		const rtOpts = {
			type: HalfFloatType,
			format: RGBAFormat,
			minFilter: LinearFilter,
			magFilter: LinearFilter,
			depthBuffer: false,
			stencilBuffer: false,
		};
		this._pingRT = new WebGLRenderTarget(1, 1, rtOpts);
		this._pingRT.texture.name = "HazeTemporal.Ping";
		this._pongRT = new WebGLRenderTarget(1, 1, rtOpts);
		this._pongRT.texture.name = "HazeTemporal.Pong";
	}

	/**
	 * The newest accumulated result. The backing buffer alternates every frame
	 * as the ping-pong swaps, so the Texture identity changes — a consumer that
	 * binds it to a uniform once would read a stale/alternating buffer. Use
	 * setOutputConsumer to rebind per frame instead.
	 */
	get texture(): Texture {
		return this._pongRT.texture;
	}

	setInputTexture(texture: Texture) {
		this._material.uniforms.uInput.value = texture;
	}

	setAlpha(alpha: number) {
		this._material.uniforms.uAlpha.value = alpha;
	}

	/**
	 * Register a downstream sampler (the composite effect) to be re-pointed at
	 * the freshly written buffer after each accumulation. Required because the
	 * output buffer alternates every frame.
	 */
	setOutputConsumer(consumer: (tex: Texture) => void) {
		this._outputConsumer = consumer;
	}

	override setSize(width: number, height: number): void {
		const w = Math.max(1, Math.round(width * this._resolutionScale));
		const h = Math.max(1, Math.round(height * this._resolutionScale));
		this._pingRT.setSize(w, h);
		this._pongRT.setSize(w, h);
	}

	override render(
		renderer: WebGLRenderer,
		_inputBuffer: WebGLRenderTarget | null,
		_outputBuffer: WebGLRenderTarget | null,
	): void {
		const src = this._material.uniforms.uInput.value;
		if (!src) return;

		// Read last frame's result (pong), blend with the fresh march into ping,
		// then swap so pong holds the newest. Reading pong while writing ping
		// avoids a read/write feedback on the same buffer.
		this._material.uniforms.uHistory.value = this._pongRT.texture;
		renderer.setRenderTarget(this._pingRT);
		renderer.render(this.scene, this.camera);

		const tmp = this._pingRT;
		this._pingRT = this._pongRT;
		this._pongRT = tmp;

		// The output Texture object just changed — notify the composite so it
		// samples this frame's result, not last frame's buffer.
		this._outputConsumer?.(this._pongRT.texture);
	}

	override dispose(): void {
		this._material.dispose();
		this._pingRT.dispose();
		this._pongRT.dispose();
		super.dispose();
	}
}
