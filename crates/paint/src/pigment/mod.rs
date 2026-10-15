//! Wet-brush pigment mixing: representative reflectances, single-constant Kubelka–Munk.
//! See docs/wet-blending.md for the research, assumptions and limits of the approximation.
//! Basis decomposition and tables adapted from Spectral.js 3.0.0 (Ronald van Wijnen, MIT;
//! see LICENSE-spectral.txt). Optical weighting uses linear paint masses here.
mod tables;

use photocraft_cms::{Builtin, CmsError, Profile, Transform, transform};
use photocraft_color::{
    PixelFormat,
    convert::{linear_to_srgb, srgb_to_linear},
};
use photocraft_raster::{from_rgba_into, to_rgba};
use std::sync::Arc;

/// Profile transforms for a wet stroke, shared by its live-preview forks.
#[derive(Clone)]
pub struct WetColorSpace {
    to_rgb: Arc<Transform>,
    from_rgb: Arc<Transform>,
    sample_to_native: Arc<Transform>,
}

impl WetColorSpace {
    /// `document` describes target pixels; `sample` describes the compositor's RGB output.
    pub fn new(document: &Profile, sample: &Profile) -> Result<Self, CmsError> {
        let options = transform::TransformOptions::default();
        Ok(Self {
            to_rgb: transform::cached(document, Builtin::Srgb.profile(), options)?,
            from_rgb: transform::cached(Builtin::Srgb.profile(), document, options)?,
            sample_to_native: transform::cached(sample, document, options)?,
        })
    }

    pub(crate) fn matches(&self, fmt: PixelFormat) -> bool {
        self.to_rgb.inputs() == fmt.mode.color_channels()
    }

    pub(crate) fn convert_sample(&self, input: &[f32], out: &mut [f32]) {
        self.sample_to_native.eval(input, out);
    }

    pub(crate) fn convert_brush(&self, input: &[f32], out: &mut [f32]) {
        self.from_rgb.eval(input, out);
    }
}

fn rgb(fmt: PixelFormat, native: &[f32], space: Option<&WetColorSpace>) -> [f32; 3] {
    if let Some(space) = space {
        let mut rgb = [0.0; 3];
        space.to_rgb.eval(native, &mut rgb);
        rgb
    } else {
        let [r, g, b, _] = to_rgba(&fmt, native);
        [r, g, b]
    }
}

/// Mix premultiplied native colors with an explicit final alpha component, even for opaque
/// formats. Opacity remains arithmetic; only nonzero paint masses enter the pigment mixture.
pub(crate) fn mix_native(fmt: PixelFormat, space: Option<&WetColorSpace>, a: &[f32], b: &[f32], t: f32, out: &mut [f32]) {
    let nc = fmt.mode.color_channels();
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 0.0 };
    if t == 0.0 || a == b {
        out.copy_from_slice(a);
        return;
    }
    if t == 1.0 {
        out.copy_from_slice(b);
        return;
    }
    let alpha = |v: &[f32]| v.get(nc).copied().filter(|a| a.is_finite()).unwrap_or(0.0).clamp(0.0, 1.0);
    let wa = f64::from(alpha(a)) * f64::from(1.0 - t);
    let wb = f64::from(alpha(b)) * f64::from(t);
    let total = wa + wb;
    out.fill(0.0);
    if total <= 0.0 {
        return;
    }
    // Transparent paint contributes no hidden RGB, and needs no color-space round trip.
    if wa == 0.0 || wb == 0.0 {
        let (color, weight) = if wa == 0.0 { (b, t) } else { (a, 1.0 - t) };
        for (dst, src) in out.iter_mut().zip(color) {
            *dst = *src * weight;
        }
        return;
    }
    let straight = |color: &[f32]| {
        let mut native = [0.0; 5];
        for (dst, src) in native.iter_mut().zip(color).take(nc) {
            *dst = *src / alpha(color);
        }
        if let Some(a) = native.get_mut(nc) {
            *a = 1.0;
        }
        rgb(fmt, &native, space)
    };
    let mixed = mix_rgb(straight(a), straight(b), wb / total);
    let mut native = [0.0; 5];
    if let Some(space) = space {
        space.from_rgb.eval(&mixed, &mut native);
    } else {
        from_rgba_into(&fmt, [mixed[0], mixed[1], mixed[2], 1.0], &mut native);
    }
    for (dst, src) in out.iter_mut().zip(native).take(nc) {
        *dst = src * total as f32;
    }
    if let Some(a) = out.get_mut(nc) {
        *a = total as f32;
    }
}

/// Linear RGB -> representative spectrum. The seven basis weights are nonnegative.
fn reflectance(rgb: [f64; 3]) -> [f64; 38] {
    let [r, g, b] = rgb.map(|v| v.clamp(0.0, 1.0));
    let w = r.min(g).min(b);
    let [r, g, b] = [r - w, g - w, b - w];
    let weights = [w, g.min(b), r.min(b), r.min(g), (r - g.max(b)).max(0.0), (g - r.max(b)).max(0.0), (b - r.max(g)).max(0.0)];
    let mut spectrum = [0.0; 38];
    for (basis, weight) in tables::BASE.iter().zip(weights) {
        for (dst, value) in spectrum.iter_mut().zip(basis) {
            *dst += value * weight;
        }
    }
    // A finite absorption floor keeps ideal RGB black from becoming infinite K/S.
    spectrum.map(|v| v.clamp(0.0001, 1.0))
}

fn linear_rgb(spectrum: &[f64; 38]) -> [f64; 3] {
    let xyz = tables::XYZ_WEIGHTS.map(|row| row.iter().zip(spectrum).map(|(w, r)| w * r).sum::<f64>());
    tables::XYZ_TO_RGB.map(|row| row.iter().zip(xyz).map(|(w, v)| w * v).sum())
}

fn mix_rgb(a: [f32; 3], b: [f32; 3], t: f64) -> [f32; 3] {
    if a == b {
        return a;
    }
    let clean = |v: f32| if v.is_finite() { v } else { 0.0 };
    // Bound the spectral part, carrying HDR/out-of-gamut values as an RGB residual.
    let a = a.map(clean);
    let b = b.map(clean);
    let ra = reflectance(a.map(|v| f64::from(srgb_to_linear(v.clamp(0.0, 1.0)))));
    let rb = reflectance(b.map(|v| f64::from(srgb_to_linear(v.clamp(0.0, 1.0)))));
    // Reflectance-derived luminance balances tinting strengths: dark blue otherwise dominates
    // yellow, and ideal black overwhelms even a trace of white. Keep paint mass linear (no
    // additional power curve on the slider or on opacity).
    let luminance = |r: &[f64; 38]| tables::XYZ_WEIGHTS[1].iter().zip(r).map(|(w, r)| w * r).sum::<f64>().max(0.0001);
    let wa = (1.0 - t) * luminance(&ra);
    let wb = t * luminance(&rb);
    let concentration = wb / (wa + wb);
    let mut mixed = [0.0; 38];
    for ((dst, a), b) in mixed.iter_mut().zip(ra).zip(rb) {
        let ks = |r: f64| (1.0 - r).powi(2) / (2.0 * r);
        let k = (1.0 - concentration) * ks(a) + concentration * ks(b);
        // Rationalized inverse: avoids cancellation for very dark paint.
        *dst = 1.0 / (1.0 + k + (k * (k + 2.0)).sqrt());
    }
    let encode = |r: [f64; 38]| linear_rgb(&r).map(|v| f64::from(linear_to_srgb(v.clamp(0.0, 1.0) as f32)));
    let (base_a, base_b, result) = (encode(ra), encode(rb), encode(mixed));
    let mut out = [0.0; 3];
    for (((((dst, m), x), y), ax), by) in out.iter_mut().zip(result).zip(a).zip(b).zip(base_a).zip(base_b) {
        // Endpoint correction prevents spectral reconstruction errors accumulating across dabs.
        let corrected = m + (1.0 - t) * (f64::from(x) - ax) + t * (f64::from(y) - by);
        *dst = corrected.clamp(-f64::from(f32::MAX), f64::from(f32::MAX)) as f32;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blue_and_yellow_make_green_and_order_does_not_matter() {
        let blue = [0.0, 0.0, 1.0];
        let yellow = [1.0, 1.0, 0.0];
        for t in [0.25, 0.5, 0.75] {
            let a = mix_rgb(blue, yellow, t);
            let b = mix_rgb(yellow, blue, 1.0 - t);
            assert!(a[1] > a[0] + 0.05 && a[1] > a[2], "{t}: {a:?}");
            for (x, y) in a.into_iter().zip(b) {
                assert!((x - y).abs() < 1e-6);
            }
        }
    }

    #[test]
    fn endpoints_identity_hdr_and_transparency_are_preserved() {
        let fmt = PixelFormat::RGBA32F;
        let a = [2.0, 0.25, -0.1, 1.0];
        let b = [0.1, 0.8, 0.2, 1.0];
        let mut out = [0.0; 4];
        for (t, expected) in [(0.0, a), (1.0, b)] {
            mix_native(fmt, None, &a, &b, t, &mut out);
            assert_eq!(out, expected);
        }
        mix_native(fmt, None, &a, &a, 0.5, &mut out);
        assert_eq!(out, a);
        mix_native(fmt, None, &a, &b, 0.2, &mut out);
        assert!(out[0] > 1.0 && out.iter().all(|v| v.is_finite()), "{out:?}");
        let transparent = [0.0, 1.0, 0.0, 0.0];
        mix_native(fmt, None, &transparent, &b, 0.25, &mut out);
        assert_eq!(out, b.map(|v| v * 0.25));
        mix_native(fmt, None, &transparent, &transparent, 0.5, &mut out);
        assert_eq!(out[3], 0.0);
    }

    #[test]
    fn mixture_is_finite_for_black_white_extremes_and_invalid_channels() {
        for a in [[0.0; 3], [1.0; 3], [f32::MAX; 3], [f32::NAN, f32::INFINITY, f32::NEG_INFINITY]] {
            for t in [0.001, 0.5, 0.999] {
                assert!(mix_rgb(a, [0.1, 0.4, 0.9], t).iter().all(|v| v.is_finite()));
            }
        }
    }

    #[test]
    fn native_icc_mixing_matches_the_srgb_reference_in_other_spaces() {
        use photocraft_color::{ColorMode, SampleType};
        let reference = mix_rgb([0.12, 0.24, 0.8], [0.9, 0.85, 0.15], 0.5);
        for (mode, builtin) in [
            (ColorMode::Rgb, Builtin::AdobeRgbCompat),
            (ColorMode::Rgb, Builtin::LinearSrgb),
            (ColorMode::Lab, Builtin::LabD50),
            (ColorMode::Cmyk, Builtin::CoatedCmyk),
        ] {
            let fmt = PixelFormat::new(mode, SampleType::F32, true);
            let space = WetColorSpace::new(builtin.profile(), Builtin::Srgb.profile()).unwrap();
            let nc = fmt.mode.color_channels();
            let mut a = vec![1.0; nc + 1];
            let mut b = a.clone();
            space.from_rgb.eval(&[0.12, 0.24, 0.8], &mut a);
            space.from_rgb.eval(&[0.9, 0.85, 0.15], &mut b);
            let mut out = a.clone();
            mix_native(fmt, Some(&space), &a, &b, 0.5, &mut out);
            let rgb = rgb(fmt, &out, Some(&space));
            assert!(out.iter().all(|v| v.is_finite()), "{mode:?}: {out:?}");
            // CMYK's profile gamut mapping need not round trip saturated colors.
            if mode != ColorMode::Cmyk {
                for (v, expected) in rgb.into_iter().zip(reference) {
                    assert!((v - expected).abs() < 0.01, "{builtin:?}: {rgb:?}, expected {reference:?}");
                }
            }
        }
    }
}
