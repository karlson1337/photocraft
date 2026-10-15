# Wet Blending

Wet Blending is an optional color mixing mode for PhotoCraft's Mixer Brush. Enable it in Brush Settings › Transfer. The Blending, Dilution and Persistence values are saved with the brush settings and can also be passed to `paint.mixerBrush`.

## Controls

Each slider uses a value from 0 to 1. The model converts those values into per-dab coefficients:

```text
B0 = 1 - Blending
D0 = 1 - Dilution
R0 = 1 - Persistence
```

- **Blending** reduces the fresh brush-color contribution as the slider increases.
- **Dilution** makes opacity follow the sampled canvas as the slider increases.
- **Persistence** controls how much of the previous dab's mixture carries into the next dab.

## How it works

For each dab, the brush samples color and opacity under its footprint. It combines that sample with the brush color using Dilution and Blending, then carries part of the previous mixture forward using Persistence. The first dab initializes the carried mixture. Each dab deposits the result using its coverage, brush flow, opacity, stroke opacity, selection and transparency lock. The carried mixture resets at the start of each stroke.

While drawing, the live preview includes the finishing dabs that releasing the pointer would add, including smoothing catch-up. These dabs use a copy of the carried mixture and working pixels, so showing the preview does not change the paint carried into the next pointer movement.

Wet mode uses subtractive pigment mixing for the brush/sample mixture and for persistence.
The default Mixer Brush reservoir model is unchanged. The spatial footprint sample remains an
alpha-weighted average: it measures the visible canvas beneath the brush. Deposition uses the
existing coverage/alpha compositing; coverage describes how much area is painted, not pigment
concentration. Selection and transparency locking keep their existing meaning.

With `Cs` the premultiplied footprint sample, `As` its opacity, `Cu` its recovered straight color
(falling back to the brush color below opacity 1/128), and `Cf, Af` the brush color and effective
dab opacity, the calculation is:

```text
Cd = lerp(Cs, Cu, D0)
Ad = lerp(As, 1, D0)
candidate = pigment_mix((Cd, Ad), (Ad * Af * Cf, Ad * Af), B0 * dab.mix)
carried = pigment_mix(previous_carried, candidate, R0)
```

`lerp(a, b, t)` interpolates from `a` to `b` by `t`. The first candidate initializes `carried`.
`pigment_mix` interpolates alpha arithmetically and normalizes pigment weights by the two
nonzero alpha contributions. Completely transparent paint contributes no hidden RGB.

## Pigment model

1. Convert straight native channels through the document ICC profile into sRGB, then linearize.
   Sample All Layers uses the profile of the compositor output. Mask/channel targets use their
   scalar gray interpretation. The lower-level `WetBlending::new` API without a color space uses
   the raster library's default native-to-sRGB conversions.
2. Reconstruct 38 representative reflectance samples (380–750 nm) using the seven basis spectra
   from Spectral.js 3.0.0. Convert each reflectance `r` into `K/S = (1-r)^2 / (2r)`.
3. Weight the two K/S spectra by paint mass times spectral luminance, then normalize. This
   balances dark and light pigments' tinting strengths. Paint mass is linear in the controls;
   no additional slider exponent is applied. Reflectance is bounded to `[0.0001, 1]` to avoid
   infinite absorption at ideal black.
4. Recover reflectance with the numerically stable inverse
   `r = 1 / (1 + K/S + sqrt((K/S)^2 + 2K/S))`, integrate with D65-weighted CIE observer values,
   and convert to sRGB and back to the document profile.

The spectral result is clipped to the sRGB gamut. An RGB residual, interpolated by paint mass,
preserves reconstruction differences and carries finite HDR/out-of-gamut values outside the
bounded spectral model. Exact endpoints and identical colors bypass reconstruction. All storage
depths use floating-point calculations; the spectral calculation runs twice per dab, not per pixel.

This is an approximation using representative spectra and equal scattering, not a prediction of
specific measured paints. RGB alone cannot identify a unique pigment spectrum. Repeated RGB
reconstruction can still drift; document pixels do not store pigment concentrations.

## Research and attribution

- [Sochorová and Jamriška, *Practical Pigment Mixing for Digital Painting* (2021)](https://scrtwpns.com/mixbox.pdf):
  motivates Kubelka–Munk mixing and RGB residuals. PhotoCraft does not include Mixbox's library,
  measured pigment palette, or lookup table. Its [distributed library](https://scrtwpns.com/mixbox/docs/#license)
  has a noncommercial license.
- [Burns, *Subtractive Color Mixture Computation*](https://arxiv.org/abs/1710.06364)
  (the paper also linked on ResearchGate): reconstructs representative spectra from RGB and
  discusses spectral geometric mixtures. PhotoCraft uses K/S mixing to include scattering.
- [Ivković et al., *Lorentz Pigment Mixing* (2025)](https://www.intechopen.com/journals/1/articles/647):
  describes nonlinear RGB transition trajectories. It was reviewed but is not the implemented
  pigment model; its channel-transition equations do not supply the spectral absorption/scattering
  representation used here.
- [Spectral.js 3.0.0](https://github.com/rvanwijnen/spectral.js/tree/3.0.0), Ronald van Wijnen:
  MIT-licensed basis spectra, observer weights and conversion matrix. Tables and license are in
  `crates/paint/src/pigment/`; this is a Rust implementation with no JavaScript runtime dependency.
