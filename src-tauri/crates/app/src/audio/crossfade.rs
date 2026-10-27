//! Crossfade DSP helpers: the equal-power gain curves.
//!
//! The track fading in is decoded by a second
//! [`waveflow_core::playback::stream::ActiveStream`], opened to the same
//! output rate and channel count as the primary, and the decoder mixes the
//! two with these gains.

/// Equal-power fade gains for a normalized progress `t ∈ [0, 1]`.
/// Returns `(fade_out_gain, fade_in_gain)` where
/// `fade_out² + fade_in² = 1` so total power is preserved across the
/// mix — gives a noticeably less "scooped" mid-fade than linear gains.
#[inline]
pub fn equal_power_gains(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    let angle = t * std::f32::consts::FRAC_PI_2;
    (angle.cos(), angle.sin())
}

#[cfg(test)]
mod tests;
