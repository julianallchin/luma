import { BlendFunction, Effect } from "postprocessing";
import { type Texture, Uniform } from "three";

const fragmentShader = /* glsl */ `
uniform sampler2D uHazeTexture;

void mainImage(const in vec4 inputColor, const in vec2 uv, out vec4 outputColor) {
  vec3 haze = texture2D(uHazeTexture, uv).rgb;
  outputColor = vec4(inputColor.rgb + haze, inputColor.a);
}
`;

export class HazeCompositeEffect extends Effect {
	constructor(hazeTexture: Texture | null = null) {
		super("HazeCompositeEffect", fragmentShader, {
			blendFunction: BlendFunction.SKIP,
			uniforms: new Map<string, Uniform>([
				["uHazeTexture", new Uniform(hazeTexture)],
			]),
		});
	}

	setHazeTexture(texture: Texture) {
		(this.uniforms.get("uHazeTexture") as Uniform).value = texture;
	}
}
