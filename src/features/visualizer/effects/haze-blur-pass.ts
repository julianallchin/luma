import { Pass } from "postprocessing";
import {
	HalfFloatType,
	LinearFilter,
	RGBAFormat,
	ShaderMaterial,
	type Texture,
	Uniform,
	Vector2,
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

// Separable Gaussian. Direction is (1,0) for horizontal pass,
// (0,1) for vertical. Hardcoded 9-tap symmetric weights.
const fragmentShader = /* glsl */ `
varying vec2 vUv;

uniform sampler2D uInput;
uniform vec2 uTexelSize;
uniform vec2 uDirection;
uniform float uRadius;

void main() {
  vec2 step = uDirection * uTexelSize * uRadius;

  // 9-tap symmetric Gaussian, sigma ~ 2 (in tap units)
  const float w0 = 0.20236;
  const float w1 = 0.179044;
  const float w2 = 0.124009;
  const float w3 = 0.067234;
  const float w4 = 0.028532;

  vec4 sum =
      texture2D(uInput, vUv               ) * w0
    + texture2D(uInput, vUv + step * 1.0  ) * w1
    + texture2D(uInput, vUv - step * 1.0  ) * w1
    + texture2D(uInput, vUv + step * 2.0  ) * w2
    + texture2D(uInput, vUv - step * 2.0  ) * w2
    + texture2D(uInput, vUv + step * 3.0  ) * w3
    + texture2D(uInput, vUv - step * 3.0  ) * w3
    + texture2D(uInput, vUv + step * 4.0  ) * w4
    + texture2D(uInput, vUv - step * 4.0  ) * w4;

  gl_FragColor = sum;
}
`;

export interface HazeBlurPassOptions {
	/** Tap stride in pixels. Default 1.5. Higher = blurrier. */
	radius?: number;
	/** RT resolution scale, should match the haze RT. Default 1.0. */
	resolutionScale?: number;
}

export class HazeBlurPass extends Pass {
	private _material: ShaderMaterial;
	private _pingRT: WebGLRenderTarget;
	private _pongRT: WebGLRenderTarget;
	private _resolutionScale: number;
	private _texelSize = new Vector2(1, 1);

	constructor(options: HazeBlurPassOptions = {}) {
		super("HazeBlurPass");
		this.needsSwap = false;
		this.needsDepthTexture = false;

		this._resolutionScale = options.resolutionScale ?? 1.0;

		this._material = new ShaderMaterial({
			name: "HazeBlurMaterial",
			vertexShader,
			fragmentShader,
			depthTest: false,
			depthWrite: false,
			uniforms: {
				uInput: new Uniform(null),
				uTexelSize: new Uniform(this._texelSize),
				uDirection: new Uniform(new Vector2(1, 0)),
				uRadius: new Uniform(options.radius ?? 1.5),
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
		this._pingRT.texture.name = "HazeBlur.Ping";
		this._pongRT = new WebGLRenderTarget(1, 1, rtOpts);
		this._pongRT.texture.name = "HazeBlur.Pong";
	}

	/** The blurred output, sampled by the composite effect. */
	get texture(): Texture {
		return this._pongRT.texture;
	}

	setInputTexture(texture: Texture) {
		this._material.uniforms.uInput.value = texture;
	}

	setRadius(radius: number) {
		this._material.uniforms.uRadius.value = radius;
	}

	override setSize(width: number, height: number): void {
		const w = Math.max(1, Math.round(width * this._resolutionScale));
		const h = Math.max(1, Math.round(height * this._resolutionScale));
		this._pingRT.setSize(w, h);
		this._pongRT.setSize(w, h);
		this._texelSize.set(1 / w, 1 / h);
	}

	override render(
		renderer: WebGLRenderer,
		_inputBuffer: WebGLRenderTarget | null,
		_outputBuffer: WebGLRenderTarget | null,
	): void {
		const sourceTex = this._material.uniforms.uInput.value;
		if (!sourceTex) return;

		const u = this._material.uniforms;

		// Horizontal pass: source -> ping
		u.uDirection.value.set(1, 0);
		u.uInput.value = sourceTex;
		renderer.setRenderTarget(this._pingRT);
		renderer.render(this.scene, this.camera);

		// Vertical pass: ping -> pong
		u.uDirection.value.set(0, 1);
		u.uInput.value = this._pingRT.texture;
		renderer.setRenderTarget(this._pongRT);
		renderer.render(this.scene, this.camera);

		// Restore so a future setInputTexture caller sees the original wiring.
		u.uInput.value = sourceTex;
	}

	override dispose(): void {
		this._material.dispose();
		this._pingRT.dispose();
		this._pongRT.dispose();
		super.dispose();
	}
}
