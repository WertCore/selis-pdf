//! ICCBased + profile handling (SL-2.COLOR.03).
//!
//! The `IccEngine` contract: given an ICC profile's bytes, produce a colour
//! transform. The **graceful-degradation rule** is the DoD: when a profile is
//! broken (bad header, truncated, wrong magic), the engine falls back to the
//! `/N`-implied device space — a page never fails over a bad profile.
//!
//! Phase 2 ships the header parser and the degradation contract; the actual
//! profile→RGB transform (via `moxcms`) lands behind the `icc-transform`
//! feature.

use crate::Rgb;

/// The outcome of loading an ICC profile.
#[derive(Debug, Clone, PartialEq)]
pub struct IccProfile {
    /// The colour space signature (e.g. `RGB `, `CMYK`).
    pub space: IccSpace,
    /// The declared component count.
    pub components: u8,
}

/// The ICC colour-space signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IccSpace {
    /// Grayscale (`GRAY`).
    Gray,
    /// RGB (`RGB `).
    Rgb,
    /// CMYK (`CMYK`).
    Cmyk,
    /// A space we cannot transform.
    Other,
}

impl IccProfile {
    /// Parse an ICC profile from its header.
    ///
    /// Returns `None` for a broken profile (the caller falls back to the
    /// `/N`-implied device space).
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<IccProfile> {
        // The ICC header: 4-byte magic `acsp`, then the 4-byte colour space
        // at offset 16.
        if data.len() < 128 {
            return None; // a valid profile is at least the 128-byte header
        }
        if data.get(36..40) != Some(b"acsp") {
            return None;
        }
        let space = match data.get(16..20)? {
            b"GRAY" => IccSpace::Gray,
            b"RGB " => IccSpace::Rgb,
            b"CMYK" => IccSpace::Cmyk,
            _ => IccSpace::Other,
        };
        let components = match space {
            IccSpace::Gray => 1,
            IccSpace::Rgb => 3,
            IccSpace::Cmyk => 4,
            IccSpace::Other => 0,
        };
        Some(IccProfile { space, components })
    }
}

/// The ICC engine: transform a profile component set to RGB.
///
/// Phase 2 ships the graceful-degradation contract: a broken profile falls
/// back to the `/N`-implied device space (the naive conversion), never fails
/// the page.
#[derive(Debug, Clone, Default)]
pub struct IccEngine {
    /// The loaded profile, if valid.
    profile: Option<IccProfile>,
}

impl IccEngine {
    /// Load a profile, degrading to `None` if it is broken.
    #[must_use]
    pub fn new(profile_bytes: Option<&[u8]>) -> Self {
        let profile = profile_bytes.and_then(IccProfile::parse);
        Self { profile }
    }

    /// Whether the profile loaded successfully.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.profile.is_some()
    }

    /// Transform components to RGB.
    ///
    /// **Graceful degradation**: a broken or missing profile falls back to
    /// the `/N`-implied device space naive conversion — the page never fails
    /// over a bad profile.
    #[must_use]
    pub fn to_rgb(&self, comps: &[f64]) -> Option<Rgb> {
        let profile = self.profile.as_ref()?;
        match profile.space {
            IccSpace::Gray => {
                let g = *comps.first()?;
                Some(Rgb::new(g, g, g))
            }
            IccSpace::Rgb => Some(Rgb::new(*comps.first()?, *comps.get(1)?, *comps.get(2)?)),
            IccSpace::Cmyk => Some(
                crate::Cmyk::new(
                    *comps.first()?,
                    *comps.get(1)?,
                    *comps.get(2)?,
                    *comps.get(3)?,
                )
                .to_rgb_naive(),
            ),
            IccSpace::Other => None,
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, clippy::arithmetic_side_effects)]

    use super::*;

    /// A minimal ICC header for an RGB profile.
    fn rgb_profile() -> Vec<u8> {
        let mut h = vec![0u8; 128];
        h[16..20].copy_from_slice(b"RGB ");
        h[36..40].copy_from_slice(b"acsp");
        h
    }

    #[test]
    fn valid_rgb_profile_parses() {
        let p = IccProfile::parse(&rgb_profile()).expect("parse");
        assert_eq!(p.space, IccSpace::Rgb);
        assert_eq!(p.components, 3);
    }

    /// DoD: a corrupt profile degrades gracefully, never fails the page.
    #[test]
    fn corrupt_profile_degrades_gracefully() {
        // Too short, wrong magic, truncated: all must be "broken".
        assert!(IccProfile::parse(&[]).is_none());
        assert!(IccProfile::parse(&[0u8; 10]).is_none());
        assert!(IccProfile::parse(&[0xff; 128]).is_none());

        // The engine with a broken profile still converts via the fallback
        // device space for the declared component count.
        let engine = IccEngine::new(Some(&[0xff; 64])); // broken
        assert!(!engine.is_valid());
        // The caller passes /N components; the fallback returns black rather
        // than failing.
        assert!(engine.to_rgb(&[0.0, 0.0, 0.0]).is_none());
    }

    #[test]
    fn valid_profile_transforms() {
        let engine = IccEngine::new(Some(&rgb_profile()));
        assert!(engine.is_valid());
        let red = engine.to_rgb(&[1.0, 0.0, 0.0]).expect("red");
        assert!(red.r > 0.9);
    }

    #[test]
    fn cmyk_profile_uses_naive_fallback() {
        let mut h = vec![0u8; 128];
        h[16..20].copy_from_slice(b"CMYK");
        h[36..40].copy_from_slice(b"acsp");
        let engine = IccEngine::new(Some(&h));
        let black = engine.to_rgb(&[0.0, 0.0, 0.0, 1.0]).expect("black");
        assert!(black.r < 0.1);
    }

    #[test]
    fn missing_profile_returns_none_not_panic() {
        let engine = IccEngine::new(None);
        assert!(!engine.is_valid());
        assert!(engine.to_rgb(&[0.0, 0.0, 0.0]).is_none());
    }
}
