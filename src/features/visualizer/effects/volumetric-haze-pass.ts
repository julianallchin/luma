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

export const MAX_LIGHTS = 16;

const FLOATS_PER_LIGHT = 16; // 4 RGBA texels per light
const TEX_WIDTH = MAX_LIGHTS * (FLOATS_PER_LIGHT / 4);

const vertexShader = /* glsl */ `
varying vec2 vUv;
void main() {
  vUv = uv;
  gl_Position = vec4(position.xy, 1.0, 1.0);
}
`;

// Self-contained raymarch shader. Writes scattered light into its own
// render target — no scene color input, no compositing. The compositing
// happens later in HazeCompositeEffect after the blur pass smooths the
// dither out.
const fragmentShader = /* glsl */ `
#define MAX_LIGHTS 16
#define LIGHT_TEXELS 4

varying vec2 vUv;

uniform sampler2D uLightData;
uniform sampler2D uDepthBuffer;
uniform int uLightCount;
uniform float uHazeDensity;
uniform float uRaySteps;
uniform int uDebugMode;
uniform mat4 uInvProjection;
uniform mat4 uInvView;
uniform vec3 uCameraPos;
uniform float uElapsed;
uniform vec2 uResolution;
uniform int uFrame;        // frame counter — drives the temporal ray-start jitter

// Physically-scaled transport controls (tunable; see constructor defaults).
uniform float uBeamGain;   // pushes the lit core well over the tonemap knee
uniform float uSatPoint;   // radiance at which the core starts whitening
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

float hazeNoise(vec3 p, float elapsed) {
  vec3 drift = vec3(elapsed * 0.4, elapsed * 0.25, elapsed * 0.15);
  vec3 q = p * 2.0 + drift;
  float n = noise3D(q) * 0.6 + noise3D(q * 3.0 + drift + 3.7) * 0.4;
  return 0.45 + 0.55 * n;
}

vec3 worldPosFromUV(vec2 uv, float rawDepth) {
  vec4 clip = vec4(uv * 2.0 - 1.0, rawDepth * 2.0 - 1.0, 1.0);
  vec4 viewPos = uInvProjection * clip;
  viewPos /= viewPos.w;
  vec4 worldPos = uInvView * viewPos;
  return worldPos.xyz;
}

struct SpotLight {
  vec3 position;
  float intensity;
  vec3 direction;
  float cosBeam;   // cosine of the inner (flat hot core) half-angle
  vec3 color;
  float range;
  float cosField;  // cosine of the outer half-angle (profile reaches zero)
  float wash;       // 0 = tight spot, 1 = wide wash (softens the phase)
};

SpotLight getLight(int idx) {
  float texW = float(MAX_LIGHTS * LIGHT_TEXELS);
  int base = idx * LIGHT_TEXELS;
  vec4 t0 = texture2D(uLightData, vec2((float(base) + 0.5) / texW, 0.5));
  vec4 t1 = texture2D(uLightData, vec2((float(base + 1) + 0.5) / texW, 0.5));
  vec4 t2 = texture2D(uLightData, vec2((float(base + 2) + 0.5) / texW, 0.5));
  vec4 t3 = texture2D(uLightData, vec2((float(base + 3) + 0.5) / texW, 0.5));

  SpotLight l;
  l.position  = t0.rgb;
  l.intensity = t0.a;
  l.direction = t1.rgb;
  l.cosBeam   = t1.a;
  l.color     = t2.rgb;
  l.range     = t2.a;
  l.cosField  = t3.r;
  l.wash      = t3.g;
  return l;
}

// Two-angle luminaire profile: flat hot core out to the beam angle, smooth
// shoulder from beam->field, zero past the field angle. The edge is a pure
// per-sample analytic angular test — sharpness is independent of raymarch
// step count, and emerges from how close beam/field sit (narrow gap = hard
// shoulder for spots, wide gap = soft shoulder for washes). Swap this for a
// sampled IES/GDTF lookup later without touching any call site.
float angularProfile(float cosAngle, float cosBeam, float cosField) {
  return smoothstep(cosField, cosBeam, cosAngle);
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

  // Bound the march to the span of the ray that can actually scatter: the union
  // of the lights' range-spheres, clipped to geometry. Without this, a beam
  // against an empty background stretches the step budget across the full
  // 30-unit clamp and undersamples the (thin) beam — the noisy case. Here every
  // step lands inside a beam's reach instead, so sampling density is high
  // whether or not something is behind the beam.
  float tNear = 1e9;
  float tFar = 0.0;
  for (int li = 0; li < MAX_LIGHTS; li++) {
    if (li >= uLightCount) break;
    SpotLight L0 = getLight(li);
    vec3 oc = uCameraPos - L0.position;
    float b = dot(oc, rayDir);
    float c = dot(oc, oc) - L0.range * L0.range;
    float disc = b * b - c;
    if (disc < 0.0) continue;            // ray misses this light's sphere
    float sq = sqrt(disc);
    float t1 = -b + sq;
    if (t1 < 0.0) continue;              // sphere entirely behind the camera
    tNear = min(tNear, max(-b - sq, 0.0));
    tFar  = max(tFar, t1);
  }
  tFar = min(tFar, min(hitDist, 30.0)); // geometry / far clamp occludes the beam

  if (tFar <= tNear) {
    // Ray never enters a beam — nothing to scatter, and no empty-space march.
    gl_FragColor = vec4(0.0, 0.0, 0.0, rawDepth);
    return;
  }

  float marchLen = tFar - tNear;
  int steps = int(uRaySteps);
  float stepSize = marchLen / float(steps);
  // Temporal jitter: advance the per-pixel ray start by a golden-ratio walk
  // each frame so successive frames sample different depths. The temporal
  // accumulation pass averages these into a clean high-step look.
  float j = fract(IGN(gl_FragCoord.xy) + float(uFrame) * 0.61803398875);
  float dither = j * stepSize;

  vec3 scattered = vec3(0.0);
  float transmittance = 1.0;

  for (int i = 0; i < 128; i++) {
    if (i >= steps) break;

    float t = tNear + dither + float(i) * stepSize;
    vec3 samplePos = uCameraPos + rayDir * t;

    float noiseVal = uDebugMode == 1 ? 0.7 : hazeNoise(samplePos, uElapsed);
    // Ambient medium fill — diffuse haze the beams cut through. Noise-textured
    // everywhere; it is not part of any beam so it never needs the core gate.
    vec3 stepScatter = vec3(0.03, 0.025, 0.02) * noiseVal * uHazeDensity;

    if (uDebugMode != 2) {
      for (int j = 0; j < MAX_LIGHTS; j++) {
        if (j >= uLightCount) break;

        SpotLight light = getLight(j);
        vec3 toLight = light.position - samplePos;
        float dist = length(toLight);
        if (dist > light.range) continue;

        vec3 L = toLight / dist;                 // sample -> source
        float cosAngle = dot(-L, light.direction);
        float angular = angularProfile(cosAngle, light.cosBeam, light.cosField);
        if (angular <= 0.0) continue;

        // Physically-scaled HDR radiance: inverse-square falloff x angular
        // profile, scaled so the core sits well over the tonemap knee. The
        // white-hot-at-source / soften-with-distance gradient is emergent from
        // 1/d^2, not a brightness constant.
        float invSq = 1.0 / max(dist * dist, uNearClamp);
        float radiance = light.intensity * angular * invSq * uBeamGain;
        // Soft range taper — fade out over the last quarter so the beam ends
        // naturally instead of popping at the hard cull sphere. With enough
        // range, 1/d^2 has usually faded it to nothing well before this.
        radiance *= 1.0 - smoothstep(light.range * 0.75, light.range, dist);

        // White-hot correction: a pure RGB primary has no energy in its zero
        // channel to clip, so it can never whiten on tonemapping alone. Inject
        // white as radiance crosses the saturation point — gated, principled.
        float whiteAmount = smoothstep(uSatPoint, uSatPoint * 3.0, radiance);
        vec3 emit = mix(light.color, vec3(1.0), whiteAmount) * radiance;

        // Single-scatter forward glow; washes scatter less directionally.
        float g = mix(uPhaseG, uPhaseG * 0.3, light.wash);
        float phase = henyeyGreenstein(dot(L, rayDir), g);

        // Noise gated on the angular core AND distance: clean near the
        // fixture, untextured in the hot core, full haze only in the dim
        // far field. Keys off the angular (pure cone) term, never radiance.
        float farFade = smoothstep(0.0, light.range * 0.35, dist);
        float lightNoise = mix(1.0, noiseVal, farFade * (1.0 - angular));

        stepScatter += emit * phase * lightNoise;
      }
    }

    float localDensity = uHazeDensity * (0.5 + 0.5 * noiseVal);
    float extinction = localDensity * 0.08;
    float stepTransmittance = exp(-extinction * stepSize);

    scattered += transmittance * stepScatter * (1.0 - stepTransmittance);
    transmittance *= stepTransmittance;
  }

  // Radiance is already physically scaled — output linear HDR for the
  // tonemap/bloom chain, no arbitrary post-multiply. Alpha carries the scene
  // depth this (possibly half-res) texel saw, so the composite can do a
  // depth-aware bilateral upsample without bleeding across silhouettes.
  gl_FragColor = vec4(scattered, rawDepth);
}
`;

export interface VolumetricHazePassOptions {
	hazeDensity?: number;
	steps?: number;
	/** RT resolution scale, 0.5 = half-res. Default 1.0. */
	resolutionScale?: number;
	/** Core radiance multiplier — push the lit core over the tonemap knee. Default 80. */
	beamGain?: number;
	/** Radiance at which the core begins to whiten. Default 20. */
	saturationPoint?: number;
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
				uRaySteps: new Uniform(options.steps ?? 4),
				uInvProjection: new Uniform(new Matrix4()),
				uInvView: new Uniform(new Matrix4()),
				uCameraPos: new Uniform(new Vector3()),
				uElapsed: new Uniform(0),
				uDebugMode: new Uniform(0),
				uResolution: new Uniform({ x: 1, y: 1 }),
				uFrame: new Uniform(0),
				// Physically-scaled transport. Tunable at runtime via
				// `pass.material.uniforms.<name>.value` while dialing in a look.
				uBeamGain: new Uniform(options.beamGain ?? 80),
				uSatPoint: new Uniform(options.saturationPoint ?? 20),
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
		const offset = index * FLOATS_PER_LIGHT;
		const buf = this.lightBuffer;
		buf[offset] = posX;
		buf[offset + 1] = posY;
		buf[offset + 2] = posZ;
		buf[offset + 3] = intensity;
		buf[offset + 4] = dirX;
		buf[offset + 5] = dirY;
		buf[offset + 6] = dirZ;
		buf[offset + 7] = cosBeam;
		buf[offset + 8] = r;
		buf[offset + 9] = g;
		buf[offset + 10] = b;
		buf[offset + 11] = range;
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
		this._material.uniforms.uResolution.value = { x: w, y: h };
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
