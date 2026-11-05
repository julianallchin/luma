# Sky clouds: prior art

Research notes for the Luma sky-cloud layer. Date: 2026-09-24.
Target: outdoor venue, camera near the ground, clouds are background.
Presets: Clear, Fair weather, Scattered, Overcast, Storm. Cheap on GPU, with a Quality Low path.

Numbers below come from the slides, papers or engine source. Where a value is from memory and not checked, the note says "unverified".

## 1. Guerrilla / Schneider: Horizon Zero Dawn (SIGGRAPH 2015)

Source: "The Real-time Volumetric Cloudscapes of Horizon Zero Dawn", Advances in Real-Time Rendering 2015.
https://www.guerrilla-games.com/read/the-real-time-volumetric-cloudscapes-of-horizon-zero-dawn
PDF: https://d3d3g8mu99pzk9.cloudfront.net/AndrewSchneider/The-Real-time-Volumetric-Cloudscapes-of-Horizon-Zero-Dawn.pdf

- Budget: about 2 ms on PS4, 20 MB of memory. It replaced all sky-dome assets.
- Noise: two 3D textures and one 2D texture.
  - 128^3 RGBA: R = Perlin-Worley, GBA = Worley at rising frequencies. Base shape.
  - 32^3 RGB: Worley at rising frequencies. Erodes the edges.
  - 128^2 RGB curl noise. Distorts the detail noise to fake turbulence.
- Weather map: R = coverage, G = precipitation, B = cloud type.
- Three "mathematical" height gradients (stratus, stratocumulus, cumulus). Blend by type.
- Volumetric low clouds between 1500 m and 4000 m. Cirrus/alto above 4000 m are 2D scrolling textures, not ray-marched.
- Cloudscape radius 35 km. Clouds near the horizon change to cumulus above 50% coverage from 15 km out.
- Coverage is applied after the height gradient. Density is lower at the cloud base, so bases are wispy.
- Tip: invert the Worley detail noise at the cloud base for wispy shapes.
- Lighting: Beer's law * Henyey-Greenstein (HG) * "powder" term. Powder darkens the edges that face the light. It only shows when the view vector is near the light vector.
- Beer-powder, as used in later public code: `E = 2 * exp(-d) * (1 - exp(-2 d))`.
- Ray march: 64 steps looking up, up to 128 at the horizon. Large cheap steps until density > 0. Then one step back and full-detail steps. Back to cheap steps after several zero samples.
- Light: 6 samples in a cone toward the sun. 5 near samples plus 1 far sample for shadows from distant clouds. Light samples use the cheap shader once alpha > 0.3. This made the shader 2x faster.
- Colour model: ambient sky term rises with height. Direct term uses the sun colour. Blend to atmosphere colour with depth.
- Cost without tricks: about 20 ms. Fix: quarter-res buffer, update 1 of 16 pixels in each 4x4 block per frame, reproject the rest. Gave 10x or more.

## 2. Nubis (SIGGRAPH 2017)

Source: "Nubis: Authoring Real-Time Volumetric Cloudscapes with the Decima Engine".
PDF: https://d3d3g8mu99pzk9.cloudfront.net/AndrewSchneider/Nubis-Authoring-Realtime-Volumetric-Cloudscapes-with-the-Decima-Engine-Final.pdf

- `remap(v, lo, hi, nlo, nhi) = nlo + (v - lo) / (hi - lo) * (nhi - nlo)` is the main shaping tool.
- Height gradient example (stratus): `remap(h, 0.0, 0.1, 0, 1) * remap(h, 0.2, 0.3, 1, 0)`.
- Perlin-Worley: `remap(perlin, 1 - worley, 1, 0, 1)`.
- Base cloud: `remap(low_freq_noise, high_freq_fbm, 1, 0, 1)`.
- Coverage as erosion: `remap(noise, coverage, 1, 0, 1)`. Then multiply by coverage.
- Wind skew with height: `p += height_fraction * wind_dir * 500`.
- Anvil: `coverage = pow(coverage, remap(h, 0.7, 0.8, 1, lerp(1, 0.5, anvil_bias)))`.
- Weather map: RG = coverage (Worley or Perlin-Worley variants), B = type (0 stratus, 0.5 stratocumulus, 1 cumulus).
- Phase: `max(HG(cos, 0.6), silver_intensity * HG(cos, 0.99 - silver_spread))`. One HG could not give both sunset highlights and bright 90-degree clouds.
- Multiple scattering (after Wrenninge): `max(exp(-d), exp(-d * 0.25) * 0.7)`. Ramp it down as the view looks toward the sun.
- In-scatter probability (replaces powder):
  `depth_prob = 0.05 + pow(lod_density, remap(h, 0.3, 0.85, 0.5, 2.0))`,
  `vertical_prob = pow(remap(h, 0.07, 0.14, 0.1, 1.0), 0.8)`.
- March: 54 to 96 steps by view angle. Switch back to cheap steps after 10 zero-density samples. 5 cone light samples with rising LOD.
- PS4 timings, clouds on half the screen: 22 ms (reprojection only) -> 8.1 ms (adaptive step + LOD) -> 3.34 ms (light samples only where density > 0) -> 1.81 ms (cull below horizon) -> 1.2 ms (depth culling with a conservative max of low-LOD depth).
- Output buffer: R = direct light, G = atmosphere blend factor, B = ambient, A = alpha. Depth for the atmosphere is taken where alpha reaches 0.5.

## 3. Nubis Evolved (SIGGRAPH 2022)

Source: "Nubis, Evolved: Real-time Volumetric Clouds for Skies, Environments, and VFX".
https://www.guerrilla-games.com/read/nubis-evolved
PDF: https://d3d3g8mu99pzk9.cloudfront.net/AndrewSchneider/NubisEvolved/NubisEvolved-NoVideos.pdf

- Sky model is "vertical profile": `dimensional_profile = vertical_profile * coverage`.
- Density: `saturate(noise_composite - (1 - dimensional_profile))`. One 128^3 4-channel noise.
- Light energy = direct scattering + ambient scattering.
- Direct = transmittance * primary phase + multiple scattering * secondary phase.
- Cheap ambient: `ambient = pow(1 - dimensional_profile, 0.5)`. Envelope clouds: `pow(1 - saturate(coarse_density), 0.25) * height_fraction`. No extra march.
- Multiple scattering volume: `remap(profile * step, 0.1, 1, 0, 1) * pow(coverage * type, 0.25)`, then scaled by powers of attenuated light and height fraction.
- Distance-based step: `step = 3.0 + 60.0 * dist / 16384.0` (metres).
- Vertical-profile sky clouds use temporal upscaling over 16 frames: 20 ms -> 2 ms. It fails for near, fast clouds.
- Sky cost: 0.5 ms with partial sky to 2.2 ms with full sky (PS5, from the 2023 talk recap).
- VFX scaling PS4 vs PS5: 960x540 vs 1920x1080; light samples 6 vs 10; view samples 60-90 vs 96-180; noise MIP 1 vs 0; about 4 ms vs 2-3 ms.

## 4. Nubis^3 voxel clouds (SIGGRAPH 2023)

Source: "Nubis Cubed: Methods (and madness) to model and render immersive real-time voxel-based clouds".
https://www.guerrilla-games.com/read/nubis-cubed
PDF: https://d3d3g8mu99pzk9.cloudfront.net/AndrewSchneider/Nubis%20Cubed.pdf

- For fly-through clouds. Voxel grids up to 512x512x64 (about 16.8 MB) with signed distance fields for step placement.
- Light: first 2 light samples in the march. The rest come from a 256x256x32 pre-computed light voxel grid.
- Keeps the 2022 ambient approximation `pow(1 - dimensional_profile, 0.5)`.
- Not needed for Luma. The camera does not enter clouds.

## 5. Hillaire: Frostbite sky, atmosphere and clouds (SIGGRAPH 2016 PBS course)

Source: "Physically Based Sky, Atmosphere and Cloud Rendering in Frostbite".
PDF: https://media.contentapi.ea.com/content/dam/eacom/frostbite/files/s2016-pbs-frostbite-sky-clouds-new.pdf

- Cloud medium: albedo about 1, so sigma_s = sigma_t. Measured extinction: 0.04-0.06 /m for stratus, 0.05-0.12 /m for cumulus.
- One slab of constant height. Weather texture: R = 2D density, G = type. Type texture (x = type, y = height): R = density profile, G = erosion amount.
- Noise stored as a single-channel 3D texture after combining. Faster than RGBA with the same look.
- Energy-conserving step (use this):
  `S_int = (S - S * exp(-sigma_t * d)) / sigma_t`, then `T *= exp(-sigma_t * d)`. Clamp sigma_t to an epsilon.
  The naive "add then attenuate" either gains or loses energy at large steps.
- Dual-lobe phase: `p = lerp(HG(g0), HG(g1), w)`. Needed so clouds with the sun behind the camera do not look dull.
- Multiple scattering (Wrenninge 2013): sum N octaves with `sigma_s * a^n`, `sigma_t * b^n`, `g * c^n`. Keep `a <= b` for energy conservation. N = 2 in their timings.
- Ambient: SH sky probe, no occlusion. Scale by a linear height gradient in [a, 1] across the layer. `a` is the ground-bounce part. Artist scale to dim it.
- Shadow samples: 4 per view sample. Distance grows by a constant factor each step. Jitter plus temporal gives soft shadows.
- Temporal: jitter sample positions each frame, blend with reprojected history (exponential moving average). 14 samples with temporal matched a much higher count.
- Cloud shadow on the world: a 2D transmittance texture, projected along the sun on a flat planet. Used for opaque, transparent, particles and GI.
- Aerial perspective on clouds: use a transmittance-weighted mean depth, then apply AP once.
- Clouds on aerial perspective: `L_AP = L_AP * T_cloud + L_cloud`. A thick layer removes sky light from the air below.
- Xbox One cost, worst case (horizon, 3/5 of screen, 16 samples, N=2): 0.91 ms at 720p, 1.60 ms at 1080p. Main view at half res.

## 6. Hillaire: scalable sky and atmosphere (EGSR 2020)

Source: "A Scalable and Production Ready Sky and Atmosphere Rendering Technique".
PDF: https://sebh.github.io/publications/egsr2020.pdf

- LUTs: transmittance LUT, multiple-scattering LUT, sky-view LUT (lat/long around camera up), aerial perspective volume.
- AP volume default: 32x32 over the screen, 32 depth slices over 32 km.
- Sky-view latitude mapping: `v = 0.5 + 0.5 * sign(l) * sqrt(|l| / (pi/2))`. More texels at the horizon.
- Multiple scattering: assume isotropic after order 2. `F_ms = 1 / (1 - f_ms)`. `Psi_ms = L_2nd * F_ms`. Stored in a small 2D LUT.
- Cost: 0.14 ms on-screen, 0.31 ms total at 1280x720 on a GTX 1080. Bruneton LUT update: 250 ms.
- Take for clouds: a lat/long sky texture around the camera is valid for a ground camera. Distant clouds can go in the same kind of texture.
- The multiple-scattering LUT and sky-view LUT give the sky colour for the cloud ambient term.

## 7. Unreal Engine Volumetric Cloud

Sources:
https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-cloud-component-in-unreal-engine
https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-cloud-component-properties-in-unreal-engine
https://dev.epicgames.com/documentation/en-us/unreal-engine/volumetric-clouds?application_version=4.27

- Layer is a spherical shell: Layer Bottom Altitude and Layer Height, in km. Defaults 5 km and 10 km (unverified, from engine source memory).
- Shape and extinction come from a volume material. Material outputs "Conservative Density" so the tracer can skip empty space before running the full material.
- Tracing Start Max Distance and Tracing Max Distance (km). Stop Tracing Transmittance Threshold ends the march early.
- `r.VolumetricCloud.DistanceToSampleMaxCount` = 15 km: the distance over which the max sample count is spread.
- Sample counts: scale factors per view, reflection, shadow. Clamped by `r.VolumetricCloud.*SampleMaxCount` cvars.
- Render target modes (`r.VolumetricRenderTarget.Mode`):
  - 0: trace at 1/4 res, reconstruct at 1/2, upsample to full. Default, best for fast motion.
  - 1: trace at 1/2 res, reconstruct at full.
  - 2: full res, no intersection with opaque meshes.
  - 3: trace 1/8 res, reconstruct at 1/2, per-pixel compose.
- Phase: two HG lobes, Phase G, Phase G2, Phase Blend.
- Multiple scattering: up to 2 extra octaves. Contribution 0.5, Occlusion 0.5, Eccentricity 0.5 by default. Docs advise 1 octave for games.
- Ground Contribution: lights cloud bases with shadowed ground light and a Ground Albedo colour.
- Sky Light Cloud Bottom Occlusion: darkens sky light at the cloud base.
- Directional light: Cast Cloud Shadows, Cloud Shadow on Atmosphere Strength, Cloud Shadow on Surface Strength, Cloud Shadow Extent (km around camera). Shadow is a Beer Shadow Map (BSM). Docs advise BSM on consoles.
- Sky light: Cloud Ambient Occlusion map (strength, extent, aperture). Real-time capture is time-sliced over 9 frames.

## 8. Unity HDRP Volumetric Clouds

Sources:
https://docs.unity3d.com/Packages/com.unity.render-pipelines.high-definition@17.0/manual/volumetric-clouds-volume-override-reference.html
https://github.com/Unity-Technologies/Graphics/blob/master/Packages/com.unity.render-pipelines.high-definition/Runtime/Lighting/VolumetricClouds/VolumetricClouds.cs
https://github.com/Unity-Technologies/Graphics/blob/master/Packages/com.unity.render-pipelines.high-definition/Runtime/Lighting/VolumetricClouds/VolumetricCloudsUtilities.hlsl

Modes: Simple (presets), Advanced (per-type maps), Manual (own cloud map + LUT).
Simple mode has a Performance and a Quality sub-mode. Quality adds micro erosion.

Preset values from `ApplyCurrentCloudPreset()` (Quality / Performance where they differ):

| Preset   | density mult | shape factor  | shape scale | erosion factor | erosion scale | micro erosion (Q) | bottom alt (m) | altitude range (m) |
|----------|--------------|---------------|-------------|----------------|---------------|-------------------|----------------|--------------------|
| Sparse   | 0.40         | 0.925 / 0.95  | 5.0         | 0.85 / 0.80    | 75 / 107      | 0.65 @ 300        | 3000           | 1000               |
| Cloudy   | 0.40         | 0.875 / 0.90  | 5.0         | 0.90 / 0.80    | 75 / 107      | 0.65 @ 300        | 1200           | 2000               |
| Overcast | 0.30         | 0.45 / 0.50   | 5.0         | 0.70 / 0.50    | 75 / 107      | 0.50 @ 300        | 1500           | 2500               |
| Stormy   | 0.35         | 0.825 / 0.85  | 5.0         | 0.90 / 0.75    | 75 / 107      | 0.60 @ 300        | 1000           | 5000               |

Height curves per preset (keyframes as (height01, value)):
- Sparse: density (0,0) (0.05,1) (0.75,1) (1,0); erosion (0,1) (0.1,0.9) (1,1); AO (0,0) (0.25,0.5) (1,0).
- Cloudy: density (0,0) (0.15,1) (1,0.1); erosion (0,1) (0.1,0.9) (1,1); AO (0,0) (0.25,0.4) (1,0).
- Overcast: density (0,0) (0.05,1) (0.9,0) (1,0); erosion (0,1) (0.1,0.9) (1,1); AO flat 0.
- Stormy: density (0,0) (0.037,1) (0.6,1) (1,0); erosion has 6 keys near 0.8-1.0; AO (0,0) (0.1,0.4) (1,0).

Note: low shape factor means less noise shaping, so Overcast is a smooth, full sheet.

Global defaults: primary steps 64 (32-1024), light steps 6 (1-32), powder 0.25, multi-scattering 0.5, scattering tint black, temporal accumulation 0.95, ghosting reduction on, shadows off, shadow distance 8000 m, ambient probe dimmer 1, sun dimmer 1, erosion occlusion 0.1, wind: cloud map speed 0.5, shape 1.0, erosion 0.25, altitude distortion 0.25.

Shader facts:
- Extinction `sigma_t = lerp(0.04, 0.12, rain)` per metre, times density. Matches the Frostbite measured range.
- `shape = lerp(0.1, 1.0, shapeFactor) * densityCurve`; base cloud is remapped by coverage, then multiplied by coverage^2.
- Phase per octave: `HG(0.7 * ms^o) + HG(-0.7 * ms^o)`. 2 octaves. ms = multiScattering.
- Light march: optical depth over `numLightSteps` samples, total length clamped to `numLightSteps * 1000 m`. First sample at 0.25 of a step. Light samples skip the erosion texture and subtract `erosionFactor * 0.1` instead.
- Multiple scattering: `sum_o exp(-tau * ms^o) * phase[o] * ms^o` (Wrenninge).
- Powder: `p = saturate((1 - exp(-4 density)) * 2)`, blended in with `smoothstep(0.5, -0.5, cosAngle)` and intensity.
- Integration: Frostbite energy-conserving form. Sun and ambient are accumulated as scalars, colour is applied at the end.
- Ambient: ambient probe convolved with a Cornette-Shanks phase, times a baked AO curve.
- Step: `min(totalDistance / numSteps, maxStepSize)`, blue-noise start offset. Coarse steps after 8 empty samples.
- Erosion texture MIP rises 0 -> 4 between 3 km and 100 km.
- Resolution: trace at 1/4 res, reconstruct at 1/2 res with a 4-sub-pixel rotation, then upscale and blend with history.

## 9. Studio Gobo: fewer steps plus jitter plus TAA (2016)

Source: Toft, Bowles, Zimmermann, "Optimisations for Real-Time Volumetric Cloudscapes". https://arxiv.org/abs/1609.05344

- 128 steps: 297.7 ms. 8 steps: 2.3 ms but broken shapes. 8 steps with per-pixel random offset: 7.5 ms, noisy. Add TAA: same cost, clean.
- They claim a similar image with 1/16 of the steps.
- The naive integration makes brightness depend on step length. An analytic per-step integral fixes it (same idea as Frostbite).
- Take: low step counts only work with jitter, the analytic step, and temporal accumulation.

## 10. Jendersie and d'Eon: approximate Mie (SIGGRAPH 2023 talk)

Source: "An Approximate Mie Scattering Function for Fog and Cloud Rendering".
https://research.nvidia.com/publication/2023-08_approximate-mie-scattering-function-fog-and-cloud-rendering
PDF: https://research.nvidia.com/labs/rtr/approximate-mie/publications/approximate-mie.pdf

- Blend of HG (the peak) and Draine (the bulk). Matches about 95% of the Mie signal (the forward half). No fogbow or glory.
- Draine: `D(a, g) = (1 - g^2) / (4 pi (1 + g^2 - 2 g cos)^1.5) * (1 + a cos^2) / (1 + a (1 + 2 g^2) / 3)`.
- `phase = (1 - wD) * HG(gHG) + wD * D(alpha, gD)`.
- Fit over droplet diameter d in 5-50 um:
  - `gHG = exp(-0.0990567 / (d - 1.67154))`
  - `gD = exp(-2.20679 / (d + 3.91029) - 0.428934)`
  - `alpha = exp(3.62489 - 8.29288 / (d + 5.52825))`
  - `wD = exp(-0.599085 / (d - 0.641583) - 0.665888)`
- For d = 20 um (my evaluation): gHG 0.995, gD 0.594, alpha 27.1, wD 0.498.
- Analytic evaluation and sampling. Cost is a few ALU ops more than two HG lobes.

## 11. Other recent work (2023-2026)

- Andrew Schneider, "The Real-time Volumetric Superstorms of Horizon Forbidden West", GDC 2022. Internal lightning flashes at near-zero cost, temporal upscaling for fast clouds. https://www.schneidervfx.com/
- "Environmental Volumetric Neural Shading of Clouds for Real-Time Rendering", PACMCGIT, July 2026. Mesh plus triplane features, rasterised, no ray march. Sky light model instead of one light. Too complex for us. https://dl.acm.org/doi/10.1145/3820020
- "Demistifying the Clouds of Borderlands 4: Adapting Volumetric Cloud Technologies to an UE5 Open World Game", GDC session. Abstract could not be read (HTTP 403). https://schedule.gdconf.com/session/demistifying-the-clouds-of-borderlands-4-adapting-volumetric-cloud-technologies-to-an-ue5-open-world-game/915656
- Sakmary 2023, "Real-time Rendering of Atmosphere and Clouds in Vulkan" (student paper, CESCG). Hillaire 2020 sky plus Nubis-style clouds in one Vulkan renderer. Useful as a small reference build. https://cescg.org/wp-content/uploads/2023/04/Sakmary-Real-time-Rendering-of-Atmosphere-and-Clouds-in-Vulkan.pdf
- I found no 2024-2026 production talk that changes the core method. The base is still: 2D coverage + height profile + 3D noise, jittered march, energy-conserving step, Wrenninge octaves, low-res trace + temporal.
- One search result claimed a GDC 2025 Hillaire talk on REDengine 4 clouds. The linked GDC Vault page is a different talk. I dropped it.

## What we took for Luma

The implementation is `src/atmosphere/clouds.rs` (presets, weather map, blue noise), `src/atmosphere/cloud_gpu.rs` (passes), `src/shaders/atmosphere_cloud_*.wgsl`, `src/shaders/cloud_shadow.wgsl` and `src/sun_shafts.rs` with `src/shaders/sun_shafts.wgsl`. Tests: `tests/render/sky_clouds.rs`, `tests/render/horizon_seam.rs`, the unit tests in `clouds.rs`.

An earlier version baked a direction-only panorama when the sun or preset changed. It was rejected: it read as a skybox, with no parallax and no moving shadows. The design below is the Unreal and HDRP structure.

1. Geometry: a spherical shell, as Unreal. The march uses the planet frame of the sky tables, so the horizon needs no special case. View rays stop at 80 km.
2. Per-frame trace from the real camera, in world space: the clouds move against the sky when the camera moves or rises. The trace buffer is half resolution on High and quarter on Low. A live frame traces one pixel of each 2x2 block, in Bayer order (HDRP's and Horizon's amortisation); the others reproject the previous frame's result through its view-projection, using the stored transmittance-weighted cloud depth, and blend at 0.35. A still frame, or one with no usable history, traces every pixel with twice the steps.
3. Jitter: a 64x64 void-and-cluster blue-noise texture made on the CPU offsets each ray's first step; the temporal blend averages it out (Gobo, HDRP).
4. Early exit at transmittance 0.003, and empty-space skip: a sample with no weather coverage returns before any 3D noise is read.
5. Weather map: 512x512 RGBA8 over a 40 to 64 km tile. R coverage, G cloud kind (stratus to cumulus), B density. Coverage is a mix of fBm at two cell sizes (so clumps differ in size), scaled by a slow region fBm: in a broken sky the local coverage runs from 0.05 to 1.95 times the preset's, so there are large clear regions and dense clusters. The result is thresholded by rank, so the preset's total coverage is exact, with a soft band of 0.6. A deck keeps a small spread around its coverage.
6. Shape: Nubis 2017. A 128^3 Perlin-Worley volume (R) with Worley fBm at 4, 8 and 16 cells (GBA), and a 32^3 Worley detail volume, both baked on the GPU once per device. Height profile: a dome, not a slab. The top of each column rises with local coverage and kind (`mix(0.3, 1, kind) * mix(0.2, 1, sqrt(coverage))`), so thin coverage makes a low cloud and the clump centre is the tallest part; the base lifts a little at thin coverage (`0.15 * kind * (1 - coverage)`), so a base narrows in. A cumulus rounds off over the top 40 to 80 % of its height. Earlier the profile was one box per kind and every cloud was a flat pancake with a vertical wall. Coverage remap `remap(noise, 1 - profile*coverage, 1, 0, 1) * coverage`; detail erosion wispy at the base and billowed above (`mix(fbm, 1 - fbm, saturate(hf*10))`). A per-preset gain (1.5 to 3) sharpens the outline of a cumulus and leaves a deck soft. It was 4 and higher; with the steep density rise at the edge, that made the silver lining a one-pixel bright rim round every cloud.
7. Wind: the weather map drifts at the preset's speed, and the shape and detail volumes drift at 0.25 and 0.6 of it, so clouds change shape as they cross the sky.
8. Integration: Hillaire 2016 energy-conserving step. Up to twice the steps for slanting rays (adaptive to the path length in the layer).
9. Light: dual-lobe HG, g 0.85 forward with weight 0.6 and -0.25 back, for the silver lining around a sun behind cloud. Three Wrenninge octaves (a = b = c = 0.5, the HDRP and Unreal defaults), The octaves attenuate by `exp(-b * tau * 0.35)`: a light-absorption scale on the sun's optical depth for single scatter, our own value. Without it the edge of a cloud (low tau) got the full forward-scatter lobe and every cloud had a bright rim when the sun was behind the camera; with it the thin edge is not brighter than the inside, and the forward glow is kept toward the sun. Plus the two-stream diffusion estimate `1 / (1 + 0.75 (1 - g) tau)` with gain 0.75/pi, so a thick sunlit cloud is white and not grey. Powder in HDRP's form, weighted away from the sun. Light march: 6 steps (4 on Low) in a growing cone (Schneider), plus one long sample.
10. Ambient: the sky table over the layer for the tops (falling to 0.4 at the base), and the ground bounce for the base.
11. Aerial perspective on cloud: the cloud fades into the air in front of it (3x the clean-air extinction). Broken cloud fades into the clear sky behind it; a deck fades into the light under it. That light is the same one the aerial volume puts into the air under the deck (next item), so the far ground and the deck meet at the horizon without a seam.
12. Cloud shadow map (Unreal, HDRP): 1024^2 on High, 512^2 on Low, over 40 km, centred on the camera and snapped to 1 km. A live frame redraws one row in four; a still redraws all. At its edge it fades to the preset's mean sun. It is read by: the scene's surfaces (the sun on the ground and stage, so shadows move); the aerial-perspective volume, per sample along each ray (light shafts through gaps); and the stage-haze sun-shaft pass. The aerial volume also adds the sunlight the deck passes on as diffuse light.
13. Surfaces fade the map to the mean sun over a pixel's ground footprint (angle x distance^2 / height): near the horizon one pixel spans kilometres, and a single tap drew black lines and stair-steps there.
14. Stage haze sun shafts: outdoor stage haze was lit by the sun in closed form, so it glowed in a roof's shadow and under overcast. A compute pass at half resolution (quarter on Low) places 24 samples (12 on Low) per ray at equal shares of the ray's scattered light (inverting the height-field optical depth in closed form), reads the stage's sun cascades and the cloud shadow map at each, and stores the fraction the sun reaches. The composite scales the haze's sun term by it, with a depth-aware 4x4 tent upsample.
15. Sky god rays: the sky pass subtracts the air's in-scattered sun that the cloud shadow map says is shadowed (Rayleigh and Mie, 16 samples with quadratic spacing out to half the shadow map, 32 on a still frame), weighted by the cloud's alpha. So shafts show in the sky through gaps. `cloud_shadow_in_air` lifts the shadow through the layer's height: air above the layer is lit. Without it, air over a cloud was drawn as shadowed and made dark blobs.
16. Lens veil: the glare veil took the sun's radiance with no cloud, so an overcast sky with the sun at the back still had glare. The CPU now estimates the sun's visibility through the layer (12 steps along the sun ray over the weather map, with the wind drift) and scales the veil's sun by it.
17. Cirrus (the Wispy preset, and faint cirrus over Fair weather and Scattered): a 2D sheet at 7 to 10 km, not a volume. Real cirrus is ice at optical depth well under 1; a march through a thin shell would cost a lot for little. This is how Unreal's and HDRP's documentation describe their high cloud layers (a 2D layer), and the idea only; the code is ours. Density is a region field (so the sheet is patchy, with clear sky) times warped value noise stretched along the wind (streak 14 to 20 km, width 1.5 to 2 km), with a fibre term across the streak. Light: the sun through the sheet times an ice phase (an even mix of HG g 0.2 and g 0.8, so the sun glows through it) plus 0.6 of the sky over it; the sheet fades into the air at the horizon. It composites behind the cumulus. The cloud shadow map dims by `exp(-tau / sun_z)`, so a cirrus sheet takes a little of the sun but casts no hard shadow.
18. Probe: the environment cube is built from the sky through a cloud panorama traced at wind time 0, so an overcast probe is the deck.
19. Exposure: a per-preset gain on the elevation-fitted exposure, so an overcast day is not murky and a storm still reads dim.

Presets as built:

| Preset | base (km) | thickness (km) | coverage | deck floor | extinction (/km) | weather feature (km) | kind | erosion | shape / detail noise (km) | edge gain | sky light | exposure gain | wind (m/s) | cirrus: altitude (km), tau, coverage |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Clear | - | - | 0 | - | - | - | - | - | - | - | 1 | 1.0 | - | none |
| Fair weather | 1.1 | 0.9 | 0.3 | 0 | 80 | 2 | 0.6-0.95 | 0.3 | 1.2 / 0.18 | 2 | 1.0 | 1.0 | 6 | 8, 0.12, 0.2 |
| Wispy | - (no cumulus) | - | 0 | - | 0 | - | - | - | - | - | 1.0 | 1.0 | 20 | 8.5, 1.0, 0.35 (streak 14, width 1.5) |
| Scattered | 1.2 | 1.8 | 0.42 | 0 | 70 | 5 | 0.35-0.8 | 0.25 | 2.0 / 0.25 | 1.5 | 0.85 | 1.1 | 8 | 9, 0.2, 0.25 |
| Overcast | 0.8 | 1.6 | 0.95 | 0.45 | 45 | 5 | 0.1-0.45 | 0.2 | 3.0 / 0.5 | 2 | 0.35 | 2.0 | 5 | none |
| Storm | 0.6 | 3.5 | 1.0 | 0.55 | 80 | 6 | 0.4-0.9 | 0.2 | 2.0 / 0.5 | 3 | 0.15 | 2.0 | 14 | none |

### Sources and licence

Unity HDRP (Unity Companion License) and Unreal Engine (Unreal EULA) source may be used only with their own engines. No code or shader text from either is in Luma; the implementation is written from the papers and talks above. The method and values that follow HDRP and Unreal come from their public documentation and talks: the pipeline shape (a low-resolution trace with temporal reprojection, a cloud shadow map read by surfaces and the atmosphere), the octave factors of 0.5, and HDRP's powder form. Where a section above gives a value read from engine source, it says so; the implementation takes no detail from engine source that is not also in those public documents.
