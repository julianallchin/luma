import type { Scene } from "three";
import { Object3D, SpotLight } from "three";

/**
 * Fixed-size pool of Three.js SpotLights shared across all fixtures.
 * Each frame, the brightest N fixtures get real scene lights.
 */

const MAX_POOL = 8;
const SHADOW_COUNT = 2;

interface PoolEntry {
	light: SpotLight;
	target: Object3D;
}

export interface LightRequest {
	posX: number;
	posY: number;
	posZ: number;
	dirX: number;
	dirY: number;
	dirZ: number;
	r: number;
	g: number;
	b: number;
	intensity: number;
	angle: number;
	distance: number;
}

let pool: PoolEntry[] = [];
let attachedScene: Scene | null = null;
let requests: LightRequest[] = [];
let activePoolSize = 6;
let shadowsEnabled = true;

export function initSpotlightPool(scene: Scene) {
	if (attachedScene === scene && pool.length > 0) return;
	disposeSpotlightPool(attachedScene);
	for (let i = 0; i < MAX_POOL; i++) {
		const light = new SpotLight(0xffffff, 0);
		light.penumbra = 0.6;
		light.decay = 1.5;
		light.shadow.mapSize.width = 512;
		light.shadow.mapSize.height = 512;
		light.shadow.camera.near = 0.1;
		light.shadow.camera.far = 20;
		light.visible = false;
		const target = new Object3D();
		light.target = target;
		scene.add(light);
		scene.add(target);
		pool.push({ light, target });
	}
	attachedScene = scene;
}

export function disposeSpotlightPool(scene: Scene | null) {
	if (!scene) return;
	for (const { light, target } of pool) {
		scene.remove(light);
		scene.remove(target);
		light.dispose();
	}
	pool = [];
	attachedScene = null;
}

export function setPoolConfig(size: number, shadows: boolean) {
	activePoolSize = Math.min(size, MAX_POOL);
	shadowsEnabled = shadows;
}

export function beginFrame() {
	requests = [];
}

export function submitLight(req: LightRequest) {
	requests.push(req);
}

export function endFrame() {
	requests.sort((a, b) => b.intensity - a.intensity);

	for (let i = 0; i < MAX_POOL; i++) {
		const entry = pool[i];
		if (!entry) continue;

		if (i >= activePoolSize) {
			entry.light.intensity = 0;
			entry.light.castShadow = false;
			entry.light.visible = false;
			continue;
		}

		const req = requests[i];

		if (req && req.intensity > 0.01) {
			entry.light.color.setRGB(req.r, req.g, req.b);
			entry.light.intensity = req.intensity;
			entry.light.angle = req.angle;
			entry.light.distance = req.distance;
			entry.light.shadow.camera.far = req.distance;
			entry.light.position.set(req.posX, req.posY, req.posZ);
			entry.target.position.set(
				req.posX + req.dirX * req.distance,
				req.posY + req.dirY * req.distance,
				req.posZ + req.dirZ * req.distance,
			);
			entry.light.castShadow =
				shadowsEnabled && i < SHADOW_COUNT && req.intensity > 0.1;
			entry.light.visible = true;
		} else {
			entry.light.intensity = 0;
			entry.light.castShadow = false;
			entry.light.visible = false;
		}
	}
}
