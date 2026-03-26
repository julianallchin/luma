import { BlendFunction, Effect, EffectAttribute } from "postprocessing";
import {
	type Camera,
	DataTexture,
	FloatType,
	Matrix4,
	NearestFilter,
	RGBAFormat,
	Uniform,
	Vector3,
	type WebGLRenderer,
	type WebGLRenderTarget,
} from "three";

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/** Max spotlights the shader can handle — must match the #define in GLSL. */
export const MAX_LIGHTS = 16;

/** Floats per light row in the data texture (RGBA texels per light). */
const FLOATS_PER_LIGHT = 16; // 4 texels × 4 components = 16 floats

/** Width of the data texture in texels. */
const TEX_WIDTH = MAX_LIGHTS * (FLOATS_PER_LIGHT / 4);

// ---------------------------------------------------------------------------
// Fragment shader
// ---------------------------------------------------------------------------

const fragmentShader = /* glsl */ `
#define MAX_LIGHTS 16
#define LIGHT_TEXELS 4

uniform sampler2D uLightData;
uniform int uLightCount;
uniform float uHazeDensity;
uniform float uRaySteps;
uniform int uDebugMode; // 0=full, 1=no noise, 2=no lights, 3=passthrough
uniform mat4 uInvProjection;
uniform mat4 uInvView;
uniform vec3 uCameraPos;
// NOTE: cameraNear, cameraFar, depthBuffer, readDepth(), getViewZ()
// are provided by the postprocessing EffectMaterial automatically.
// Elapsed time is stored in the last float of the light data texture.

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

float getElapsed() {
  // Elapsed time stored in the very last float of the data texture (light 31, component 15 = texel 127, channel A)
  float u = (float(MAX_LIGHTS * LIGHT_TEXELS - 1) + 0.5) / float(MAX_LIGHTS * LIGHT_TEXELS);
  return texture2D(uLightData, vec2(u, 0.5)).a;
}

float hazeNoise(vec3 p, float elapsed) {
  vec3 drift = vec3(elapsed * 0.4, elapsed * 0.25, elapsed * 0.15);
  vec3 q = p * 2.0 + drift;
  float n = noise3D(q) * 0.6 + noise3D(q * 3.0 + drift + 3.7) * 0.4;
  return 0.45 + 0.55 * n;
}

// ---- world position from depth ---------------------------------------------

vec3 worldPosFromUV(vec2 uv, float rawDepth) {
  // rawDepth is [0,1] from readDepth(), which is the NDC z in [0,1]
  // Convert UV and depth to clip space [-1,1]
  vec4 clip = vec4(uv * 2.0 - 1.0, rawDepth * 2.0 - 1.0, 1.0);
  // Clip → view
  vec4 viewPos = uInvProjection * clip;
  viewPos /= viewPos.w;
  // View → world
  vec4 worldPos = uInvView * viewPos;
  return worldPos.xyz;
}

// ---- light data texture access ---------------------------------------------

struct SpotLight {
  vec3 position;
  float intensity;
  vec3 direction;
  float coneAngle;
  vec3 color;
  float range;
  float softness;
  float wash;
};

SpotLight getLight(int idx) {
  // Read all 4 texels at once (4 texture fetches instead of 13)
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
  l.coneAngle = t1.a;
  l.color     = t2.rgb;
  l.range     = t2.a;
  l.softness  = t3.r;
  l.wash      = t3.g;
  return l;
}

// ---- light evaluation ------------------------------------------------------

float lightContribution(SpotLight light, vec3 p) {
  vec3 toLight = light.position - p;
  float dist = length(toLight);
  if (dist > light.range) return 0.0;

  vec3 dir = toLight / dist;
  // light.direction points down the beam (away from the fixture)
  float cosAngle = dot(-dir, light.direction);
  float cosCone = cos(light.coneAngle);

  // Distance attenuation — linear falloff feels more natural for haze
  float atten = 1.0 - smoothstep(0.0, light.range, dist);

  if (light.wash > 0.5) {
    // ---- Wash / par / pixel mode ----
    // Soft gradient, no hard cutoff
    float cosHalf = cos(light.coneAngle);
    float gradient = smoothstep(cosHalf * 0.5, 1.0, cosAngle);
    return gradient * atten * light.intensity;
  } else {
    // ---- Spot / mover mode ----
    if (cosAngle < cosCone * 0.9) return 0.0; // slight slack for penumbra

    float penumbraWidth = (1.0 - cosCone) * light.softness;
    float edge = smoothstep(cosCone - penumbraWidth * 0.5, cosCone + penumbraWidth, cosAngle);

    return edge * atten * light.intensity * 5.0;
  }
}

// ---- interleaved gradient noise (low-discrepancy, less banding than white) --

float IGN(vec2 fragCoord) {
  return fract(52.9829189 * fract(0.06711056 * fragCoord.x + 0.00583715 * fragCoord.y));
}

// ---- main ------------------------------------------------------------------

void mainImage(const in vec4 inputColor, const in vec2 uv, out vec4 outputColor) {
  if (uHazeDensity < 0.001 || uDebugMode == 3) {
    outputColor = inputColor;
    return;
  }

  float elapsed = getElapsed();

  vec3 farWorld = worldPosFromUV(uv, 0.99);
  vec3 rayDir = normalize(farWorld - uCameraPos);

  float rawDepth = readDepth(uv);
  vec3 worldHit = worldPosFromUV(uv, rawDepth);
  float hitDist = length(worldHit - uCameraPos);
  float rayLen = clamp(hitDist, 0.1, 30.0);

  int steps = int(uRaySteps);
  float stepSize = rayLen / float(steps);
  float dither = IGN(gl_FragCoord.xy) * stepSize;

  vec3 scattered = vec3(0.0);
  float transmittance = 1.0;

  for (int i = 0; i < 8; i++) {
    if (i >= steps) break;

    float t = dither + float(i) * stepSize;
    vec3 samplePos = uCameraPos + rayDir * t;

    float noiseVal = uDebugMode == 1 ? 0.7 : hazeNoise(samplePos, elapsed);

    // Ambient haze — visible smoke in the air
    vec3 stepScatter = vec3(0.03, 0.025, 0.02) * noiseVal * uHazeDensity;

    // Scattering from light sources
    if (uDebugMode != 2) {
      for (int j = 0; j < MAX_LIGHTS; j++) {
        if (j >= uLightCount) break;

        SpotLight light = getLight(j);

        float contrib = lightContribution(light, samplePos);
        if (contrib > 0.0) {
          stepScatter += light.color * contrib * noiseVal;
        }
      }
    }

    float localDensity = uHazeDensity * (0.5 + 0.5 * noiseVal);
    float extinction = localDensity * 0.08;
    float stepTransmittance = exp(-extinction * stepSize);

    scattered += transmittance * stepScatter * (1.0 - stepTransmittance);
    transmittance *= stepTransmittance;
  }

  outputColor = vec4(inputColor.rgb + scattered * 5.0, inputColor.a);
}
`;

// ---------------------------------------------------------------------------
// Effect class
// ---------------------------------------------------------------------------

export interface VolumetricHazeOptions {
	/** Base haze density (0–1, modulated by hazer DMX). Default 0.5. */
	hazeDensity?: number;
	/** Number of raymarch steps. More = better quality, worse perf. Default 24. */
	steps?: number;
}

export class VolumetricHazeEffect extends Effect {
	readonly lightBuffer: Float32Array;
	readonly lightDataTexture: DataTexture;
	private _camera: Camera | null = null;
	private _tmpVec3 = new Vector3();

	constructor(options: VolumetricHazeOptions = {}) {
		const lightBuffer = new Float32Array(MAX_LIGHTS * FLOATS_PER_LIGHT);

		const dataTexture = new DataTexture(
			lightBuffer,
			TEX_WIDTH,
			1,
			RGBAFormat,
			FloatType,
		);
		dataTexture.minFilter = NearestFilter;
		dataTexture.magFilter = NearestFilter;
		dataTexture.needsUpdate = true;

		super("VolumetricHazeEffect", fragmentShader, {
			blendFunction: BlendFunction.NORMAL,
			attributes: EffectAttribute.DEPTH,
			uniforms: new Map<string, Uniform>([
				["uLightData", new Uniform(dataTexture)],
				["uLightCount", new Uniform(0)],
				["uHazeDensity", new Uniform(options.hazeDensity ?? 0.5)],
				["uRaySteps", new Uniform(options.steps ?? 24)],
				["uInvProjection", new Uniform(new Matrix4())],
				["uInvView", new Uniform(new Matrix4())],
				["uCameraPos", new Uniform(new Vector3())],
				["uDebugMode", new Uniform(0)],
			]),
		});

		this.lightBuffer = lightBuffer;
		this.lightDataTexture = dataTexture;
	}

	set mainCamera(camera: Camera) {
		this._camera = camera;
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
		coneAngle: number,
		r: number,
		g: number,
		b: number,
		range: number,
		softness: number,
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
		buf[offset + 7] = coneAngle;
		buf[offset + 8] = r;
		buf[offset + 9] = g;
		buf[offset + 10] = b;
		buf[offset + 11] = range;
		buf[offset + 12] = softness;
		buf[offset + 13] = wash;
		buf[offset + 14] = 0;
		buf[offset + 15] = 0;
	}

	commitLights(count: number, elapsed: number) {
		(this.uniforms.get("uLightCount") as Uniform).value = count;
		// Store elapsed time in the last float of the buffer — read by shader
		this.lightBuffer[MAX_LIGHTS * FLOATS_PER_LIGHT - 1] = elapsed;
		this.lightDataTexture.needsUpdate = true;
	}

	update(
		_renderer: WebGLRenderer,
		_inputBuffer: WebGLRenderTarget,
		_deltaTime?: number,
	) {
		const camera = this._camera;
		if (!camera) return;

		// Ensure matrices are up to date
		camera.updateWorldMatrix(true, false);

		// Inverse projection and view matrices for world-space ray reconstruction
		const invProj = (this.uniforms.get("uInvProjection") as Uniform)
			.value as Matrix4;
		const invView = (this.uniforms.get("uInvView") as Uniform).value as Matrix4;

		invProj.copy(camera.projectionMatrixInverse);
		invView.copy(camera.matrixWorld);

		camera.getWorldPosition(this._tmpVec3);
		(this.uniforms.get("uCameraPos") as Uniform).value.copy(this._tmpVec3);
	}

	dispose() {
		this.lightDataTexture.dispose();
	}
}
