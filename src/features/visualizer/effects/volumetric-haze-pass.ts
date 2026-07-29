import { Pass } from "postprocessing";
import {
	type Camera,
	DataTexture,
	FloatType,
	HalfFloatType,
	LinearFilter,
	Matrix4,
	NearestFilter,
	RGBAFormat,
	ShaderMaterial,
	type Texture,
	Uniform,
	Vector3,
	type WebGLRenderer,
	WebGLRenderTarget,
} from "three";

export const MAX_LIGHTS = 256;

const FLOATS_PER_LIGHT = 16; // 4 RGBA texels per light
const TEX_WIDTH = MAX_LIGHTS * (FLOATS_PER_LIGHT / 4);

const vertexShader = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = vec4(position.xy, 1.0, 1.0);
}
`;

// Self-contained volumetric scattering shader. Writes scattered light into
// its own render target — no scene color input, no compositing. The
// compositing happens later in HazeCompositeEffect.
//
// Architecture: there is no global march. Each light's contribution is the
// 1D integral of single-scatter along the exact span of the ray inside that
// light's cone∩range volume (an analytic ray/convex-solid intersection), and
// the integral is estimated with equiangular sampling — samples distributed
// so their density cancels the 1/d² falloff. Consequences:
//   - empty pixels cost a handful of intersection tests, zero march steps;
//   - sample budget concentrates exactly where radiance peaks (the hot
//     near-field), so the core and the edge at the lens are noise-free;
//   - sample positions vary continuously with the pixel, so there are no
//     stepped slab artifacts at any sample count.
const fragmentShader = /* glsl */ `
#define MAX_LIGHTS 256
#define LIGHT_TEXELS 4
#define MAX_SAMPLES 32

varying vec2 vUv;

uniform sampler2D uLightData;
uniform sampler2D uDepthBuffer;
uniform int uLightCount;
uniform float uHazeDensity;
uniform float uRaySteps;   // equiangular samples per beam
uniform int uDebugMode;
uniform mat4 uInvProjection;
uniform mat4 uInvView;
uniform vec3 uCameraPos;
uniform float uElapsed;
uniform int uFrame;        // frame counter — drives the temporal sample jitter

// Physically-scaled transport controls (tunable; see constructor defaults).
uniform float uBeamGain;   // pushes the lit core well over the tonemap knee
uniform float uWhiteLeak;  // broadband spill fraction of the source spectrum
uniform float uPhaseG;     // Henyey-Greenstein anisotropy (forward scatter)
uniform float uNearClamp;  // min squared-distance, avoids the 1/d^2 singularity

float readDepth(const in vec2 uv) {
  return texture2D(uDepthBuffer, uv).r;
}

// ---- 3D noise for floating haze --------------------------------------------

vec3 hash3(vec3 p) {
  p = vec3(dot(p, vec3(127.1, 311.7, 74.7)),
           dot(p, vec3(269.5, 183.3, 246.1)),
           dot(p, vec3(113.5, 271.9, 124.6)));
  return -1.0 + 2.0 * fract(sin(p) * 43758.5453123);
}

float noise3D(vec3 p) {
  vec3 i = floor(p);
  vec3 f = fract(p);
  vec3 u = f * f * (3.0 - 2.0 * f);

  return mix(mix(mix(dot(hash3(i + vec3(0,0,0)), f - vec3(0,0,0)),
                     dot(hash3(i + vec3(1,0,0)), f - vec3(1,0,0)), u.x),
                 mix(dot(hash3(i + vec3(0,1,0)), f - vec3(0,1,0)),
                     dot(hash3(i + vec3(1,1,0)), f - vec3(1,1,0)), u.x), u.y),
             mix(mix(dot(hash3(i + vec3(0,0,1)), f - vec3(0,0,1)),
                     dot(hash3(i + vec3(1,0,1)), f - vec3(1,0,1)), u.x),
                 mix(dot(hash3(i + vec3(0,1,1)), f - vec3(0,1,1)),
                     dot(hash3(i + vec3(1,1,1)), f - vec3(1,1,1)), u.x), u.y), u.z);
}

// Multiplicative density field of the medium, centered on 1. The same
// turbulence exists everywhere — including the near-field. It reads clean at
// the source anyway because the core is overexposed: a ±30% density wiggle on
// a blown-out radiance is invisible after the display transform, while the
// same wiggle on the dim tail is obvious. Emergent, like the white core — no
// spatial gate anywhere.
float hazeNoise(vec3 p, float elapsed) {
  vec3 drift = vec3(elapsed * 0.4, elapsed * 0.25, elapsed * 0.15);
  vec3 q = p * 2.0 + drift;
  float n = noise3D(q) * 0.6 + noise3D(q * 3.0 + drift + 3.7) * 0.4;
  return max(1.0 + 1.1 * n, 0.05);
}

vec3 worldPosFromUV(vec2 uv, float rawDepth) {
  vec4 clip = vec4(uv * 2.0 - 1.0, rawDepth * 2.0 - 1.0, 1.0);
  vec4 viewPos = uInvProjection * clip;
  viewPos /= viewPos.w;
  vec4 worldPos = uInvView * viewPos;
  return worldPos.xyz;
}

// Texel layout per light (see setLight): 0 = pos.xyz + range, 1 = dir.xyz +
// cosBeam, 2 = color.rgb + intensity, 3 = cosField + wash. Texel 0 alone
// drives the cheap sphere reject, so a pixel a light doesn't reach costs one
// texture read for that light, not four — what makes a 256-light loop viable.
vec4 lightTexel(int idx) {
  return texture2D(uLightData, vec2((float(idx) + 0.5) / float(MAX_LIGHTS * LIGHT_TEXELS), 0.5));
}

// Peaked photometric profile with GDTF beam/field semantics: intensity is a
// Gaussian of angle that reads 100% on the axis, 50% at the beam angle, and
// is smoothly cut to zero approaching the field angle. A real beam in haze is
// hottest down its central axis and feathers continuously toward the edge —
// a flat-top profile reads as a synthetic wedge and shows hard boundary
// lines wherever two cones overlap. This function is the gobo seam: replace
// it with a texture lookup in cone-local polar coordinates (default = this
// smooth circle) to project arbitrary gobo shapes through the volume without
// touching any call site.
float angularProfile(float cosAngle, float cosBeam, float cosField) {
  if (cosAngle <= cosField) return 0.0;
  // (1-cos) scales as angle² — so this ratio is (theta/thetaBeam)², exactly
  // the Gaussian argument. exp(-ln2 · t) puts the 50% point at the beam angle.
  float t = (1.0 - cosAngle) / max(1.0 - cosBeam, 1e-5);
  float peak = exp(-0.6931472 * t);
  // Smooth cutoff over the outer shoulder so the profile actually reaches
  // zero at the field angle instead of trailing a faint Gaussian skirt.
  float cut = smoothstep(cosField, mix(cosField, cosBeam, 0.35), cosAngle);
  return peak * cut;
}

// Henyey-Greenstein phase, normalized so isotropic (g=0) == 1 rather than
// the radiometric 1/4pi. Our light "intensity" is a 0..1 dimmer, not a
// radiance in watts, so the absolute scale lives in uBeamGain; here we only
// want the *relative* angular shape (forward glow vs. side vs. back).
float henyeyGreenstein(float cosT, float g) {
  float g2 = g * g;
  float denom = 1.0 + g2 - 2.0 * g * cosT;
  return (1.0 - g2) / pow(max(denom, 1e-4), 1.5);
}

float IGN(vec2 fragCoord) {
  return fract(52.9829189 * fract(0.06711056 * fragCoord.x + 0.00583715 * fragCoord.y));
}

void main() {
  vec2 uv = vUv;
  float rawDepth = readDepth(uv);

  if (uHazeDensity < 0.001 || uDebugMode == 3) {
    gl_FragColor = vec4(0.0, 0.0, 0.0, rawDepth);
    return;
  }

  vec3 farWorld = worldPosFromUV(uv, 0.99);
  vec3 rayDir = normalize(farWorld - uCameraPos);

  vec3 worldHit = worldPosFromUV(uv, rawDepth);
  float hitDist = length(worldHit - uCameraPos);

  // Mean extinction of the medium. The haze noise modulates in-scatter in the
  // far field only; transmittance uses the mean so it stays an analytic
  // exp(-sigma*t) — no per-step accumulation, no transmittance flicker.
  float sigma = uHazeDensity * 0.06;

  // Golden-ratio temporal walk on the per-pixel stratum jitter. With the
  // equiangular estimator the residual variance is small; the temporal pass
  // averages it out without the snap gate ever firing in the hot core.
  float j = fract(IGN(gl_FragCoord.xy) + float(uFrame) * 0.61803398875);

  vec3 scattered = vec3(0.0);

  // ---- Ambient medium fill — diffuse haze the beams cut through. Closed-form
  // transmittance; eight stratified noise taps keep the slow drifting smoke
  // structure visible instead of averaging it flat.
  {
    float ambEnd = min(hitDist, 24.0);
    float ambStep = ambEnd / 8.0;
    float amb = 0.0;
    for (int i = 0; i < 8; i++) {
      float t = (float(i) + j) * ambStep;
      float nz = uDebugMode == 1 ? 1.0 : hazeNoise(uCameraPos + rayDir * t, uElapsed);
      amb += nz * exp(-sigma * t);
    }
    scattered += vec3(0.014, 0.011, 0.009) * uHazeDensity * amb * sigma * ambStep;
  }

  if (uDebugMode != 2) {
    int sampleCount = int(clamp(uRaySteps, 1.0, float(MAX_SAMPLES)));
    // MIS split: equiangular samples own the hot near-field (their density
    // cancels 1/d² exactly), uniform samples own the dim far tail (where the
    // haze turbulence lives — equiangular alone starves it and the smoke
    // texture averages away). Balance-heuristic weights combine them.
    int nEq = (sampleCount + 1) / 2;
    int nUn = sampleCount - nEq;

    for (int li = 0; li < MAX_LIGHTS; li++) {
      if (li >= uLightCount) break;

      // ---- Exact span of this ray inside the light's cone∩range volume.
      // Range sphere first — the one-texel reject for the (common) empty pixel.
      vec4 l0 = lightTexel(li * LIGHT_TEXELS);
      vec3 oc = uCameraPos - l0.xyz;
      float range = l0.w;
      float b = dot(oc, rayDir);
      float oo = dot(oc, oc);
      float discS = b * b - (oo - range * range);
      if (discS <= 0.0) continue;
      float sq = sqrt(discS);
      float s0 = max(-b - sq, 0.0);
      float s1 = min(-b + sq, hitDist);   // geometry occludes the beam
      if (s1 <= s0) continue;

      vec4 l1 = lightTexel(li * LIGHT_TEXELS + 1);   // dir.xyz, cosBeam
      vec4 l2 = lightTexel(li * LIGHT_TEXELS + 2);   // color.rgb, intensity
      vec4 l3 = lightTexel(li * LIGHT_TEXELS + 3);   // cosField, wash
      vec3 lDir = l1.xyz;
      float cosBeam = l1.w;
      float cosField = l3.x;
      float wash = l3.y;

      // Forward-cone quadratic: dot(X-A,V)^2 = cos²(field)·|X-A|².
      float cf2 = cosField * cosField;
      float dv = dot(rayDir, lDir);
      float ov = dot(oc, lDir);
      float qa = dv * dv - cf2;
      float qb = dv * ov - cf2 * b;
      float qc = ov * ov - cf2 * oo;

      float r0 = s0;
      float r1 = s0;
      if (abs(qa) > 1e-6) {
        float qd = qb * qb - qa * qc;
        if (qd > 0.0) {
          float qs = sqrt(qd);
          r0 = (-qb - qs) / qa;
          r1 = (-qb + qs) / qa;
          if (r0 > r1) { float tmp = r0; r0 = r1; r1 = tmp; }
          r0 = clamp(r0, s0, s1);
          r1 = clamp(r1, s0, s1);
        }
      } else if (abs(qb) > 1e-6) {
        // Ray grazing along the cone surface — quadratic degenerates to linear.
        r0 = clamp(-qc / (2.0 * qb), s0, s1);
        r1 = r0;
      }

      // The solid forward cone and the range ball are both convex, so the ray
      // is inside their intersection along a single contiguous span. Partition
      // [s0,s1] at the (clamped, ordered) cone roots and keep the sub-intervals
      // whose midpoints are inside the forward cone.
      float tA = 1e9;
      float tB = -1e9;
      for (int k = 0; k < 3; k++) {
        float ea = k == 0 ? s0 : (k == 1 ? r0 : r1);
        float eb = k == 0 ? r0 : (k == 1 ? r1 : s1);
        if (eb - ea < 1e-5) continue;
        vec3 mp = oc + rayDir * ((ea + eb) * 0.5);   // midpoint relative to light
        float mm = dot(mp, lDir);
        if (mm > 0.0 && mm * mm >= cf2 * dot(mp, mp)) {
          tA = min(tA, ea);
          tB = max(tB, eb);
        }
      }
      if (tB <= tA) continue;
      float segLen = tB - tA;

      // Equiangular substitution t = delta + h·tan(theta): sample density
      // proportional to 1/d² around the source.
      float delta = -b;                                  // t of closest approach
      float h = sqrt(max(oo - b * b, uNearClamp));       // doubles as the 1/d² guard
      float thA = atan((tA - delta) / h);
      float thB = atan((tB - delta) / h);
      float dTh = thB - thA;

      // Single-scatter forward glow; washes scatter less directionally.
      float g = mix(uPhaseG, uPhaseG * 0.3, wash);
      vec3 acc = vec3(0.0);

      // Emitted spectrum: the saturated color plus a small broadband leak —
      // a real fixture is a white source behind an imperfect filter, plus lens
      // glare. White-hot is EMERGENT from this: near the source the leak's
      // absolute radiance is enormous, all channels blow out, and the AgX
      // display transform rolls the core to white; mid-beam the leak is
      // invisible and the true color shows. No radiance gate, no white mix.
      vec3 tint = mix(l2.rgb, vec3(1.0), uWhiteLeak);

      // Decorrelate the jitter across lights so overlapping cones dither
      // independently — correlated jitter turns overlaps into visible stripes.
      float jl = fract(j + float(li) * 0.7548777);

      for (int i = 0; i < MAX_SAMPLES; i++) {
        if (i >= sampleCount) break;
        float t;
        if (i < nEq) {
          float u = (float(i) + jl) / float(nEq);
          t = delta + h * tan(thA + u * dTh);
        } else {
          float u = (float(i - nEq) + jl) / float(nUn);
          t = tA + u * segLen;
        }
        // Balance heuristic over the two strategies. pdfEq uses the same
        // (clamped-h) geometry the tan-mapping actually sampled with.
        float dt2 = (t - delta) * (t - delta) + h * h;
        float misW = 1.0 / (float(nEq) * h / (dTh * dt2) + float(nUn) / segLen);

        vec3 q = oc + rayDir * t;                        // sample relative to light
        float d2 = dot(q, q);
        float dist = sqrt(d2);
        float cosAngle = dot(q, lDir) / max(dist, 1e-4);

        float angular = angularProfile(cosAngle, cosBeam, cosField);
        if (angular <= 0.0) continue;

        // Soft range taper — the beam dissolves into the dark over the last
        // stretch instead of popping at the hard cull sphere.
        float taper = 1.0 - smoothstep(range * 0.7, range, dist);

        // True HDR radiance — inverse-square, no clamp to display range. The
        // tonemapper at the end of the chain is the camera; blinding values
        // are its problem, and the white-hot core is its (correct) answer.
        float radiance = l2.a * angular * taper * uBeamGain / max(d2, uNearClamp);

        // The turbulent density field modulates in-scatter uniformly — the
        // overexposed core hides it, the dim tail reveals it (see hazeNoise).
        float nz = uDebugMode == 1 ? 1.0 : hazeNoise(uCameraPos + rayDir * t, uElapsed);

        // dot(sample->source, rayDir) = -(b + t)/dist, since q = oc + t·rayDir.
        float phase = henyeyGreenstein(-(b + t) / max(dist, 1e-4), g);
        acc += tint * (radiance * phase * nz * exp(-sigma * t) * misW);
      }

      scattered += acc * sigma;
    }
  }

  // Radiance is already physically scaled — output linear HDR for the
  // tonemap/bloom chain, no arbitrary post-multiply. Alpha carries the scene
  // depth this (possibly low-res) texel saw, so the composite can do a
  // depth-aware bilateral upsample without bleeding across silhouettes.
  gl_FragColor = vec4(scattered, rawDepth);
}
`;

export interface VolumetricHazePassOptions {
	hazeDensity?: number;
	/** Equiangular samples per beam (shader cap 32). Default 8. */
	steps?: number;
	/** RT resolution scale, 0.5 = half-res. Default 1.0. */
	resolutionScale?: number;
	/** Core radiance multiplier — push the lit core over the tonemap knee. Default 180. */
	beamGain?: number;
	/** Broadband spill fraction of the source spectrum (dichroic leak + lens
	 *  glare). White-hot emerges from this leak blowing out under HDR exposure
	 *  near the source — there is no explicit whitening. Default 0.03. */
	whiteLeak?: number;
	/** Henyey-Greenstein anisotropy, 0..1 forward. Default 0.6. */
	phaseG?: number;
	/** Min squared-distance clamp for 1/d^2. Default 0.06 (~0.25m). */
	nearClamp?: number;
}

export class VolumetricHazePass extends Pass {
	readonly lightBuffer: Float32Array;
	readonly lightDataTexture: DataTexture;
	private lastCommittedLightBuffer = new Float32Array(
		MAX_LIGHTS * FLOATS_PER_LIGHT,
	);
	private lastCommittedLightCount = -1;
	private _camera: Camera | null = null;
	private _tmpVec3 = new Vector3();
	private _material: ShaderMaterial;
	private _resolutionScale: number;
	readonly renderTarget: WebGLRenderTarget;

	constructor(options: VolumetricHazePassOptions = {}) {
		super("VolumetricHazePass");
		this.needsSwap = false;
		this.needsDepthTexture = true;

		this._resolutionScale = options.resolutionScale ?? 1.0;

		const lightBuffer = new Float32Array(MAX_LIGHTS * FLOATS_PER_LIGHT);
		const lightDataTexture = new DataTexture(
			lightBuffer,
			TEX_WIDTH,
			1,
			RGBAFormat,
			FloatType,
		);
		lightDataTexture.minFilter = NearestFilter;
		lightDataTexture.magFilter = NearestFilter;
		lightDataTexture.needsUpdate = true;

		this.lightBuffer = lightBuffer;
		this.lightDataTexture = lightDataTexture;

		this.renderTarget = new WebGLRenderTarget(1, 1, {
			type: HalfFloatType,
			format: RGBAFormat,
			minFilter: LinearFilter,
			magFilter: LinearFilter,
			depthBuffer: false,
			stencilBuffer: false,
		});
		this.renderTarget.texture.name = "VolumetricHaze.Target";

		this._material = new ShaderMaterial({
			name: "VolumetricHazeMaterial",
			vertexShader,
			fragmentShader,
			depthTest: false,
			depthWrite: false,
			uniforms: {
				uLightData: new Uniform(lightDataTexture),
				uDepthBuffer: new Uniform(null),
				uLightCount: new Uniform(0),
				uHazeDensity: new Uniform(options.hazeDensity ?? 0.5),
				uRaySteps: new Uniform(options.steps ?? 8),
				uInvProjection: new Uniform(new Matrix4()),
				uInvView: new Uniform(new Matrix4()),
				uCameraPos: new Uniform(new Vector3()),
				uElapsed: new Uniform(0),
				uDebugMode: new Uniform(0),
				uFrame: new Uniform(0),
				// Physically-scaled transport. Tunable at runtime via
				// `pass.material.uniforms.<name>.value` while dialing in a look.
				uBeamGain: new Uniform(options.beamGain ?? 180),
				uWhiteLeak: new Uniform(options.whiteLeak ?? 0.03),
				uPhaseG: new Uniform(options.phaseG ?? 0.6),
				uNearClamp: new Uniform(options.nearClamp ?? 0.06),
			},
		});

		this.fullscreenMaterial = this._material;
	}

	/** Exposed to downstream passes as their input texture. */
	get texture(): Texture {
		return this.renderTarget.texture;
	}

	get material(): ShaderMaterial {
		return this._material;
	}

	override set mainCamera(camera: Camera) {
		this._camera = camera;
	}

	override setDepthTexture(depthTexture: Texture): void {
		this._material.uniforms.uDepthBuffer.value = depthTexture;
	}

	setLight(
		index: number,
		posX: number,
		posY: number,
		posZ: number,
		intensity: number,
		dirX: number,
		dirY: number,
		dirZ: number,
		cosBeam: number,
		cosField: number,
		r: number,
		g: number,
		b: number,
		range: number,
		wash: number,
	) {
		// Texel layout (must match the shader's lightTexel reads): texel 0 is
		// pos+range so the per-pixel sphere reject costs a single fetch.
		const offset = index * FLOATS_PER_LIGHT;
		const buf = this.lightBuffer;
		buf[offset] = posX;
		buf[offset + 1] = posY;
		buf[offset + 2] = posZ;
		buf[offset + 3] = range;
		buf[offset + 4] = dirX;
		buf[offset + 5] = dirY;
		buf[offset + 6] = dirZ;
		buf[offset + 7] = cosBeam;
		buf[offset + 8] = r;
		buf[offset + 9] = g;
		buf[offset + 10] = b;
		buf[offset + 11] = intensity;
		buf[offset + 12] = cosField;
		buf[offset + 13] = wash;
		buf[offset + 14] = 0;
		buf[offset + 15] = 0;
	}

	commitLights(count: number, elapsed: number) {
		this._material.uniforms.uLightCount.value = count;
		this._material.uniforms.uElapsed.value = elapsed;
		// Advance the temporal-jitter frame counter. Wrap to keep float(uFrame)
		// precise in the shader over long sessions.
		this._material.uniforms.uFrame.value =
			(this._material.uniforms.uFrame.value + 1) % 4096;

		if (
			count !== this.lastCommittedLightCount ||
			!buffersEqual(this.lightBuffer, this.lastCommittedLightBuffer)
		) {
			this.lastCommittedLightBuffer.set(this.lightBuffer);
			this.lastCommittedLightCount = count;
			this.lightDataTexture.needsUpdate = true;
		}
	}

	override setSize(width: number, height: number): void {
		const w = Math.max(1, Math.round(width * this._resolutionScale));
		const h = Math.max(1, Math.round(height * this._resolutionScale));
		this.renderTarget.setSize(w, h);
	}

	override render(
		renderer: WebGLRenderer,
		_inputBuffer: WebGLRenderTarget | null,
		_outputBuffer: WebGLRenderTarget | null,
	): void {
		const camera = this._camera;
		if (!camera) return;

		camera.updateWorldMatrix(true, false);
		const u = this._material.uniforms;
		(u.uInvProjection.value as Matrix4).copy(camera.projectionMatrixInverse);
		(u.uInvView.value as Matrix4).copy(camera.matrixWorld);
		camera.getWorldPosition(this._tmpVec3);
		(u.uCameraPos.value as Vector3).copy(this._tmpVec3);

		renderer.setRenderTarget(this.renderTarget);
		renderer.render(this.scene, this.camera);
	}

	override dispose(): void {
		this._material.dispose();
		this.renderTarget.dispose();
		this.lightDataTexture.dispose();
		super.dispose();
	}
}

function buffersEqual(a: Float32Array, b: Float32Array) {
	for (let i = 0; i < a.length; i++) {
		if (a[i] !== b[i]) return false;
	}
	return true;
}
