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
  float coneAngle;
  vec3 color;
  float range;
  float softness;
  float wash;
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
  l.coneAngle = t1.a;
  l.color     = t2.rgb;
  l.range     = t2.a;
  l.softness  = t3.r;
  l.wash      = t3.g;
  return l;
}

float lightContribution(SpotLight light, vec3 p) {
  vec3 toLight = light.position - p;
  float dist = length(toLight);
  if (dist > light.range) return 0.0;

  vec3 dir = toLight / dist;
  float cosAngle = dot(-dir, light.direction);
  float cosCone = cos(light.coneAngle);
  float atten = 1.0 - smoothstep(0.0, light.range, dist);

  if (light.wash > 0.5) {
    float cosHalf = cos(light.coneAngle);
    float gradient = smoothstep(cosHalf * 0.5, 1.0, cosAngle);
    return gradient * atten * light.intensity;
  } else {
    if (cosAngle < cosCone * 0.9) return 0.0;
    float penumbraWidth = (1.0 - cosCone) * light.softness;
    float edge = smoothstep(cosCone - penumbraWidth * 0.5, cosCone + penumbraWidth, cosAngle);
    return edge * atten * light.intensity * 5.0;
  }
}

float IGN(vec2 fragCoord) {
  return fract(52.9829189 * fract(0.06711056 * fragCoord.x + 0.00583715 * fragCoord.y));
}

void main() {
  vec2 uv = vUv;

  if (uHazeDensity < 0.001 || uDebugMode == 3) {
    gl_FragColor = vec4(0.0, 0.0, 0.0, 1.0);
    return;
  }

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

    float noiseVal = uDebugMode == 1 ? 0.7 : hazeNoise(samplePos, uElapsed);
    vec3 stepScatter = vec3(0.03, 0.025, 0.02) * noiseVal * uHazeDensity;

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

  // Match the *5.0 boost the old shader applied at the composite step
  gl_FragColor = vec4(scattered * 5.0, 1.0);
}
`;

export interface VolumetricHazePassOptions {
	hazeDensity?: number;
	steps?: number;
	/** RT resolution scale, 0.5 = half-res. Default 1.0. */
	resolutionScale?: number;
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
		this._material.uniforms.uLightCount.value = count;
		this._material.uniforms.uElapsed.value = elapsed;

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
