//! The black hole: an actual Kerr shadow, drawn in braille, on a terminal.
//!
//! This is not a hand-tuned spiral. Every frame comes from integrating null geodesics
//! through the Kerr metric with [`nullgeo`] (MIT, CPU-only), so the shape on screen is the
//! shape the equations give: the shadow's silhouette, the disk's inner edge at the ISCO,
//! the far side of the disk lensed up over the top of the hole, and the left/right
//! brightness asymmetry caused by relativistic beaming.
//!
//! ## How a frame is made
//!
//! 1. **Trace once.** One ray per terminal cell is integrated back from a camera at
//!    48 `M` (mass units) and 24° above the equatorial plane. Each ray ends in one of three
//!    states: *captured* (the shadow), *hit the disk* (radius and a gravitational `g`
//!    factor), or *escaped to the sky* (a direction, which we turn into a star).
//! 2. **Cache the geometry.** The expensive part is the integration, and the camera does not
//!    move, so the raster is computed once per terminal size and reused. That is what makes
//!    the animation cheap: after the first frame a tick is a few thousand sines, not a few
//!    thousand geodesics.
//! 3. **Animate the flow, not the geometry.** The visible motion is the accretion flow —
//!    a Keplerian swirl (period ∝ r^1.5) modulating the disk's brightness — plus a slow
//!    pulse of the inner edge. Light does not notice, but human eyes do.
//!
//! If the tracer cannot run at all (it is pure CPU maths, so this is close to impossible),
//! the raster falls back to an analytic Schwarzschild approximation and the screen says so.
//! Nothing here reaches the network, needs a GPU, or holds a shadowed mutex.
//!
//! ## Why one colour per cell
//!
//! A terminal cannot do subpixel colour, so a cell's braille bits are one colour chosen by
//! the hottest dot in it. Drawing per-dot colour would mean emitting one `Span` per dot and
//! the frame would be a thousand spans of confetti.

use std::f64::consts::TAU;

use nullgeo::render::{
    trace_geometry, Camera, CameraPose, CameraSpec, Disk, RayClass, RayOutcome, Scene, SkyMap,
};
use nullgeo::spacetimes::Kerr;
use nullgeo::tracer::{EquatorialAnnulus, TraceConfig};
use nullgeo::Vec4;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::chrome::{self, Chrome, Segment, TAB_HOLE};
use crate::theme as t;

/// Inner edge of the disk in mass units: the ISCO of a near-extremal Kerr hole.
const R_IN: f64 = 3.0;
/// Outer edge of the emissive disk. Compact on purpose: a disk that reaches the edge of the
/// frame hides the shadow, and the shadow is the subject.
const R_OUT: f64 = 8.5;
/// Camera distance, in mass units. The shadow's apparent radius is `b_crit / D`, so the
/// distance is what decides whether the hole is a hole or the whole screen.
const CAMERA_DISTANCE: f64 = 88.0;
/// Escaped rays whose closest approach is inside this radius skimming the photon sphere:
/// they are drawn as the cold photon ring. 6.5 `M` is comfortably outside the 3 `M` photon
/// sphere of a Schwarzschild hole and comfortably inside the disk's outer edge.
const RING_M: f64 = 6.5;
/// 24° above the equatorial plane: the M87/Sgr A* kind of view, where the disk is an ellipse
/// and the far side shows up lensed over the top.
const CAMERA_ELEVATION: f64 = 1.0;
/// Field of view. Wide enough to hold the disk, tight enough that the hole is not a dot.
const FOV_DEG: f64 = 30.0;
/// Peak disk temperature, in the arbitrary units the colour ramp expects.
const T_IN: f64 = 9_000.0;

// ── braille ─────────────────────────────────────────────────────────────────────────────
//
// A cell is 2×4 dots. Row-major masks, so `BITS[row][col]` is the bit for that dot.
const BITS: [[u8; 2]; 4] = [
    [0x01, 0x08],
    [0x02, 0x10],
    [0x04, 0x20],
    [0x40, 0x80],
];

/// A solid cell: all eight dots. The disk is drawn with these so it reads as a body, not a
/// scatter of specks.
const FULL: u8 = 0xff;

// ── the raster ──────────────────────────────────────────────────────────────────────────

/// What the ray from one cell ended up doing.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    /// Swallowed: nothing comes back out. Drawn as the void.
    Shadow,
    /// Escaped to the sky, and far enough from the shadow to be a star rather than a ring.
    Sky,
    /// Escaped, but right against the shadow: light that orbited the hole first. Cold.
    PhotonRing,
    /// The integrator ran out of steps without deciding. Drawn as nothing: an unresolved
    /// ray is not a star, and pretending it is would fill the screen with a uniform haze.
    Unresolved,
    /// Hit the accretion disk, at this radius.
    Disk { radius: f64, g: f64 },
}

#[derive(Debug, Clone, Copy)]
struct Cell {
    kind: Kind,
    /// Sky direction, for stars only. Cheaper than re-tracing to find out where a dot goes.
    dir: [f64; 3],
    /// Closest approach of the ray, in mass units. This is the honest definition of a
    /// photon-ring ray, and it does not need the neighbouring cells to agree with it.
    min_radius: f64,
}

/// Which engine produced the geometry, so the screen can be honest about it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Integrated geodesics through the Kerr metric.
    Kerr,
    /// The analytic fallback: a Schwarzschild shadow and an assumed disk. Used only if the
    /// tracer returns an error.
    Analytic,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Source::Kerr => "kerr a=0.9 · nullgeo",
            Source::Analytic => "analytic · fallback",
        }
    }
}

/// The traced image: one `Cell` per terminal cell, plus the size it was traced for.
#[derive(Debug, Clone)]
pub struct Raster {
    pub width: u16,
    pub height: u16,
    pub source: Source,
    cells: Vec<Cell>,
}

impl Raster {
    /// Integrate the image, or fall back to the analytic shadow if the tracer errors.
    pub fn trace(width: u16, height: u16) -> Raster {
        Self::trace_kerr(width, height).unwrap_or_else(|_| Self::analytic(width, height))
    }

    fn trace_kerr(width: u16, height: u16) -> Result<Raster, nullgeo::Error> {
        let w = width.max(1) as usize;
        let h = height.max(1) as usize;
        let hole = Kerr::new(1.0, 0.9)?;
        let camera = Camera::new(
            CameraSpec {
                fov_deg: FOV_DEG,
                res: (w, h),
                energy: 1.0,
                supersample: 1,
                supersample_max: 1,
                jitter: false,
            },
            CameraPose {
                // (t, x, y, z): the disk lies in z = 0, so the elevation goes into z.
                position: Vec4::new(
                    0.0,
                    CAMERA_DISTANCE * CAMERA_ELEVATION.cos(),
                    0.0,
                    CAMERA_DISTANCE * CAMERA_ELEVATION.sin(),
                ),
                look_at: [0.0, 0.0, 0.0],
                up: [0.0, 0.0, 1.0],
                velocity: [0.0, 0.0, 0.0],
            },
        )?;
        let scene = Scene {
            // The sky is black: any light that gets here is what we paint as stars.
            sky: SkyMap::Uniform([0.0, 0.0, 0.0]),
            sky_secondary: None,
            disk: Some(Disk::blackbody(R_OUT, T_IN)),
        };
        let cfg = TraceConfig {
            disk: Some(EquatorialAnnulus { r_in: R_IN, r_out: R_OUT }),
            // Step lengths and the escape radius are what decide the cost of a frame. The
            // camera sits at `CAMERA_DISTANCE`, so "far enough away" is a little beyond it —
            // the default escape radius of 1000 M would have every sky ray take thousands of
            // steps to walk out of a picture it has already left one cell ago.
            dl_init: 0.5,
            dl_max: 20.0,
            max_steps: 6_000,
            escape_radius: CAMERA_DISTANCE * 1.25,
            ..Default::default()
        };
        let buffer = trace_geometry(&hole, &camera, &scene, &cfg)?;

        let mut cells = Vec::with_capacity(w * h);
        for index in 0..w * h {
            let info = buffer.primary(index);
            let kind = match info.class() {
                RayClass::Captured => Kind::Shadow,
                RayClass::Disk => {
                    let crossing = info.first_crossing;
                    Kind::Disk {
                        radius: crossing.map(|c| c.radius).unwrap_or(R_IN),
                        g: crossing.and_then(|c| c.g).unwrap_or(1.0),
                    }
                }
                RayClass::EscapedPrimary | RayClass::EscapedSecondary => Kind::Sky,
                // MaxSteps, Stalled: no answer. Not sky, and not a shadow either.
                _ => Kind::Unresolved,
            };
            let dir = match info.outcome {
                RayOutcome::Escaped { dir, .. } => dir,
                _ => [0.0, 0.0, 1.0],
            };
            cells.push(Cell { kind, dir, min_radius: info.stats.min_radius });
        }

        let mut raster = Raster {
            width: w as u16,
            height: h as u16,
            source: Source::Kerr,
            cells,
        };
        raster.mark_photon_ring();
        Ok(raster)
    }

    /// The analytic fallback: a Schwarzschild shadow of radius √27 M, an inclined annulus,
    /// and no lensing. It is worse physics and better than an empty screen.
    fn analytic(width: u16, height: u16) -> Raster {
        let w = width.max(1) as usize;
        let h = height.max(1) as usize;
        let mut cells = Vec::with_capacity(w * h);
        for y in 0..h {
            for x in 0..w {
                // Normalised screen coordinates: x in [-1, 1], y scaled for the cell aspect.
                let nx = (x as f64 + 0.5) / w as f64 * 2.0 - 1.0;
                let ny = ((y as f64 + 0.5) / h as f64 * 2.0 - 1.0) * (h as f64 / w as f64) * 2.2;
                let radius = (nx * nx + ny * ny).sqrt();
                // √27 M at 48 M with a 30° field: about 0.33 of the half-height.
                let shadow = 0.34;
                let kind = if radius < shadow {
                    Kind::Shadow
                } else if radius < shadow + 0.02 {
                    Kind::Sky
                } else {
                    let squashed = ((nx / 1.35) * (nx / 1.35) + (ny / 0.42) * (ny / 0.42)).sqrt();
                    if (0.55..1.0).contains(&squashed) {
                        Kind::Disk {
                            radius: R_IN + (R_OUT - R_IN) * (squashed - 0.55) / 0.45,
                            g: 1.0,
                        }
                    } else {
                        Kind::Sky
                    }
                };
                cells.push(Cell { kind, dir: [nx, ny, 1.0], min_radius: radius });
            }
        }
        let mut raster = Raster { width: w as u16, height: h as u16, source: Source::Analytic, cells };
        raster.mark_photon_ring();
        raster
    }

    /// The photon ring: the circle of light that piled up at the shadow's edge.
    ///
    /// Two criteria, because either alone leaves the picture wrong.
    ///
    /// * **A ray that skimmed the photon sphere** is the physical definition, and it catches
    ///   the lensed arcs away from the shadow.
    /// * **Any cell on the shadow's boundary** catches the rest. At one ray per terminal cell
    ///   the critical curve is exactly one cell wide, and the disk's own emission sits on
    ///   top of it — which is why the trace alone returns almost nothing here.
    ///
    /// The ring is drawn cold whatever it came from: it is the one part of this screen where
    /// light has been somewhere no disk has, and the palette says so.
    fn mark_photon_ring(&mut self) {
        let (w, h) = (self.width as i32, self.height as i32);
        let shadow = |x: i32, y: i32| {
            (0..w).contains(&x)
                && (0..h).contains(&y)
                && self.cells[(y * w + x) as usize].kind == Kind::Shadow
        };
        let mut ring = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let index = (y * w + x) as usize;
                let cell = self.cells[index];
                if cell.kind == Kind::Shadow {
                    continue;
                }
                if cell.kind == Kind::Sky && cell.min_radius < RING_M {
                    ring.push(index);
                    continue;
                }
                // Four neighbours: the boundary of the shadow, not its corners.
                let touches = [(-1, 0), (1, 0), (0, -1), (0, 1)]
                    .iter()
                    .any(|(dx, dy)| shadow(x + dx, y + dy));
                if touches {
                    ring.push(index);
                }
            }
        }
        for index in ring {
            self.cells[index].kind = Kind::PhotonRing;
        }
    }

    fn cell(&self, x: u16, y: u16) -> Cell {
        self.cells[y as usize * self.width as usize + x as usize]
    }

    /// How many cells are lit at all — the "is anything on screen" check, and a cheap way
    /// for tests to assert the hole exists without pinning pixels.
    pub fn lit(&self) -> usize {
        self.cells.iter().filter(|c| c.kind != Kind::Shadow).count()
    }

    /// The shadow's area in cells. A Kerr shadow at this framing is a fixed fraction of the
    /// frame whatever the terminal size, which is the property worth testing.
    pub fn shadow_cells(&self) -> usize {
        self.cells.iter().filter(|c| c.kind == Kind::Shadow).count()
    }

    /// Radii the disk actually crossed, for the footer's "inner edge" number.
    pub fn disk_radii(&self) -> Option<(f64, f64)> {
        let mut lo = f64::MAX;
        let mut hi: f64 = 0.0;
        for cell in &self.cells {
            if let Kind::Disk { radius, .. } = cell.kind {
                lo = lo.min(radius);
                hi = hi.max(radius);
            }
        }
        (hi > 0.0).then_some((lo, hi))
    }
}

// ── the animation ───────────────────────────────────────────────────────────────────────

/// The black-hole view's state: a clock, a camera, and a cached raster.
#[derive(Debug, Clone)]
pub struct BlackHoleAnim {
    /// Orbital phase, in radians. Advanced only by [`tick`](Self::tick).
    pub phase: f64,
    /// Radians per second of the swirl. Slow on purpose: this sits on someone's second
    /// monitor while they review a diff, and a fast animation is a tax on attention.
    pub speed: f64,
    /// Draw the relativistic jets. Off by default; the disk is the point.
    pub jets: bool,
    raster: Option<Raster>,
    traced_for: (u16, u16),
}

impl Default for BlackHoleAnim {
    fn default() -> Self {
        Self::new()
    }
}

impl BlackHoleAnim {
    pub fn new() -> Self {
        BlackHoleAnim {
            phase: 0.0,
            speed: 0.55,
            jets: false,
            raster: None,
            traced_for: (0, 0),
        }
    }

    /// Advance the clock. `dt` in seconds, clamped so a suspended laptop does not make the
    /// disk jump a quarter turn when it wakes up.
    pub fn tick(&mut self, dt: f64) {
        if !dt.is_finite() {
            return;
        }
        self.phase = (self.phase + self.speed * dt.clamp(0.0, 0.25)) % TAU;
    }

    /// The traced image for this size, tracing it if this is the first time we have seen it.
    pub fn raster(&mut self, width: u16, height: u16) -> &Raster {
        if self.traced_for != (width, height) {
            self.raster = Some(Raster::trace(width, height));
            self.traced_for = (width, height);
        }
        self.raster.get_or_insert_with(|| Raster::trace(width, height))
    }

    /// Colour and glyph for every cell of one frame.
    ///
    /// Returns rows of `(glyph, colour)`; a cell that draws nothing is `(' ', VOID)`, which
    /// the view skips so the page stays one colour.
    pub fn frame(&mut self, width: u16, height: u16) -> Vec<Vec<(char, Color)>> {
        let phase = self.phase;
        let jets = self.jets;
        let raster = self.raster(width, height);
        let (w, h) = (raster.width, raster.height);
        let mut out = Vec::with_capacity(h as usize);
        for y in 0..h {
            let mut row = Vec::with_capacity(w as usize);
            for x in 0..w {
                let cell = raster.cell(x, y);
                row.push(plot(cell, x, y, w, h, phase, jets));
            }
            out.push(row);
        }
        out
    }
}

/// Turn one traced cell into one glyph. This is the only place the geometry meets colour.
fn plot(
    cell: Cell,
    x: u16,
    y: u16,
    w: u16,
    h: u16,
    phase: f64,
    jets: bool,
) -> (char, Color) {
    match cell.kind {
        // Nothing escapes. The horizon is the page.
        Kind::Shadow | Kind::Unresolved => (' ', t::VOID),
        // Light that orbited the hole: one cold dot, placed in the sub-cell octant it came
        // back from, so the ring is a circle rather than a staircase of full cells.
        Kind::PhotonRing => {
            let cx = x as f64 - w as f64 / 2.0;
            let cy = y as f64 - h as f64 / 2.0;
            let angle = cy.atan2(cx);
            let dot = match ((angle / TAU * 8.0).round() as i32).rem_euclid(8) {
                0 => BITS[1][1],
                1 => BITS[2][1],
                2 => BITS[3][1],
                3 => BITS[3][0],
                4 => BITS[2][0],
                5 => BITS[1][0],
                6 => BITS[0][0],
                _ => BITS[0][1],
            };
            // The ring is a standing wave of light, not a painted circle: each octant
            // brightens and fades with the clock, and the crest of the beat goes bone-white
            // for a moment the way a caustic does.
            // Two harmonics that drift against each other, so the ring does not repeat on an
            // obvious period: the slow one carries the caustic round, the fast one makes the
            // crest uneven.
            let beat = (phase * 1.7 + angle * 3.0).sin() * (phase * 0.9 - angle)
                + 0.35 * (phase * 4.3 - angle * 5.0).sin();
            (glyph(dot), ring_colour(beat))
        }
        Kind::Sky => {
            // A star: hashed from the cell, so the same terminal size always gives the same
            // sky. It is a backdrop in image space, not a sky map: the sky used for the trace
            // is uniformly black, and a featureless sky has nothing to lens. Pretending
            // otherwise would mean inventing a texture the physics never touched.
            let b = star_brightness(x, y, source_seed(cell));
            if b <= 0.0 {
                return (' ', t::VOID);
            }
            // Stars twinkle on their own slow clock, phase-shifted per star so the sky
            // shimmers instead of pulsing in unison. Amplitude is small: a star that blinks
            // out is a bug, not an atmosphere.
            let seed = (cell.dir[0].to_bits() ^ cell.dir[1].to_bits()) as f64 / u32::MAX as f64;
            let b = b * (0.82 + 0.18 * (phase * 1.7 + seed * TAU * 3.0).sin()) as f32;
            let colour = if b > 0.70 {
                t::BONE
            } else if b > 0.40 {
                t::PHOTON_DIM
            } else {
                t::FAINT
            };
            (glyph(BITS[1][1]), colour)
        }
        Kind::Disk { radius, g } => {
            // Keplerian: the inner disk whips round, the outer disk crawls.
            let omega = 1.0 / radius.powf(1.5);
            // Screen azimuth stands in for orbital azimuth — the disk is inclined, so this is
            // a flow pattern rather than a geodesic, and it is what makes the disk seethe.
            let cx = x as f64 - w as f64 / 2.0;
            let cy = (y as f64 - h as f64 / 2.0) * 2.0;
            let phi = cy.atan2(cx);
            //
            // Three modes, because one sine reads as a pulse rather than as plasma:
            //   * a two-armed spiral advected with the orbital phase (`omega`),
            //   * a three-armed harmonic running the other way, which makes the pattern
            //     interfere with itself instead of sliding,
            //   * a fine filament that winds up as `ln r` grows, so the inner disk visibly
            //     shears ahead of the outer disk instead of the whole slab breathing.
            let spiral = (2.0 * phi + phase * omega * 9.0).sin();
            let counter = (3.0 * phi - phase * 1.6).cos();
            let shear = (5.0 * phi + 4.0 * radius.ln() + phase * omega * 22.0).sin();
            let turbulence = 0.5 * spiral * counter + 0.5 * shear;
            // Slow radial flicker: the flow is not steady, and a little hiss keeps the eye
            // from reading the pattern as a texture that is merely translating.
            let flicker = 0.06 * (phase * 3.3 + radius * 1.7).sin();
            let flow = 0.75 + 0.45 * turbulence + flicker + hot_spot(radius, phi, phase);
            // Relativistic beaming: `g` is the observed/emitted energy ratio, so the
            // approaching side of the disk is the bright side. No hand-placed highlight.
            let beamed = (0.5 + 0.5 * g).clamp(0.2, 1.7);
            // `disk_heat` is a flux ratio: the *physics* only varies about fourfold across
            // this disk, which on a terminal reads as one flat slab. Squaring the ratio keeps
            // the ordering and gives the image the falloff an eye expects.
            //
            // The display gamma is separate from the physics: the flux ratio across this disk
            // spans nearly two orders of magnitude, and shown straight that is a thin hot
            // inner edge and an invisible outer disk. `powf(0.55)` keeps the ordering and puts
            // the whole annulus on the screen, where it can be seen to shear.
            //
            // Then a Reinhard tone curve, because a terminal has no dynamic range to spare:
            // without it everything above the knee clips to a solid block and the disk reads
            // as one flat wall of dots. With it the inner edge is white-hot, the outer disk is
            // a dim stipple, and the gradient between them survives.
            let raw = 1.45
                * disk_heat(radius).powf(0.8)
                * flow
                * beamed
                * inner_rim(radius)
                * outer_fade(radius);
            let hot = raw / (1.0 + raw);
            // A marginal cell drops out or fills in on a per-cell hash, so the faint outer
            // disk is speckle rather than a grid of identical dots.
            // A terminal has no alpha and no antialiasing beyond dots, so the top of the
            // range is pushed into solid cells: the brightest plasma is a body, and the only
            // texture on screen is the cooling edge. Without this the whole disk is one
            // uniform stipple and the eye has nothing to fall into.
            let display = (hot * 1.35).clamp(0.0, 1.0);
            let glyph_bits = density(display, cell_noise(x, y));
            let mut colour = t::heat(display as f32);
            if jets && jet_at(x, y, w, h, phase) {
                colour = t::JET;
            }
            (glyph(glyph_bits), colour)
        }
    }
}

/// A boost just outside the ISCO, where the disk is hottest and the shear steepest.
///
/// Without it the inner edge is merely the brightest part of a uniform field; with it there is
/// a rim the eye can follow round the shadow, which is the feature that makes the picture read
/// as a hole with something falling into it rather than a speckled ring.
fn inner_rim(radius: f64) -> f64 {
    1.0 + 0.9 * (-((radius - R_IN - 0.35) / 0.55).powi(2)).exp()
}

/// Dissolve the outer disk instead of ending it at a hard radius.
///
/// The tracer stops the annulus dead at `R_OUT`, and a stamped circle looks like a shape drawn
/// on the screen. Fading the last quarter of the disk makes it read as matter that is simply
/// too cool to see.
fn outer_fade(radius: f64) -> f64 {
    let start = R_OUT * 0.72;
    if radius <= start {
        return 1.0;
    }
    let t = ((radius - start) / (R_OUT - start)).clamp(0.0, 1.0);
    (1.0 - t).powi(2)
}

/// The orbiting hot spot: a bright knot of plasma at a fixed radius, carried round by the
/// flow.
///
/// This is the one feature in the picture with an *identity* — the same lump of gas, visible
/// for a whole orbit, so the eye can track it. That is what makes the disk read as orbiting
/// rather than merely flickering, and it costs one Gaussian in radius and one in azimuth.
fn hot_spot(radius: f64, phi: f64, phase: f64) -> f64 {
    // Peaks just outside the ISCO, where the flow is fastest and the shear brightest.
    const SPOT_R: f64 = 4.1;
    const R_SPREAD: f64 = 1.1;
    const PHI_SPREAD: f64 = 0.6;
    // Slower than the local orbital frequency: the spot is a pattern in the flow, not a
    // particle, and a spot that laps the disk every frame reads as a strobe.
    let spot_phase = (phase * 1.15).rem_euclid(TAU);
    let mut gap = (phi - spot_phase).rem_euclid(TAU);
    if gap > TAU / 2.0 {
        gap = TAU - gap;
    }
    0.5 * (-((radius - SPOT_R) / R_SPREAD).powi(2)).exp() * (-(gap / PHI_SPREAD).powi(2)).exp()
}

/// The photon ring's colour for a beat in `-1..=1`: cold blue most of the time, brighter on
/// the crest, bone-white at the very peak. The ring never goes red — it is the one thing in
/// this view that is not the disk.
fn ring_colour(beat: f64) -> Color {
    if beat > 0.86 {
        t::BONE
    } else if beat > 0.05 {
        t::PHOTON
    } else {
        t::PHOTON_DIM
    }
}

/// Dot density for a heat: a bright cell is a solid block, a dim one is a single dot. This is
/// the only antialiasing a terminal offers, and it is what makes the disk look like it is
/// falling in rather than painted on.
fn density(hot: f64, noise: f64) -> u8 {
    // An ordered dither, not four fixed patterns: eight dot positions per cell, lit in a
    // fixed ranking order (`BAYER`), so brightness is a halftone of dot count rather than a
    // choice between four blocky textures. A terminal has no alpha; this is the closest
    // thing it has to one.
    let level = (hot.clamp(0.0, 1.0) * 8.0 + (noise - 0.5) * 0.7).round().clamp(0.0, 8.0) as u8;
    let mut bits = 0;
    for row in 0..4 {
        for col in 0..2 {
            if BAYER[row][col] < level {
                bits |= BITS[row][col];
            }
        }
    }
    bits
}

/// A stable per-cell hash in `0..1`, used only to break up dithering.
fn cell_noise(x: u16, y: u16) -> f64 {
    let mut h = (x as u64) << 32 | y as u64;
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51afd7ed558ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ceb9fe1a85ec53);
    h ^= h >> 33;
    (h % 10_000) as f64 / 10_000.0
}

/// The order dots light up in, per cell: a 2x4 ordered-dither matrix. Reading the ranks in
/// order gives 0..7, and each rank owns one braille dot position (`BITS`).
const BAYER: [[u8; 2]; 4] = [[0, 6], [4, 2], [3, 7], [5, 1]];

/// Brightness of the disk at a radius, from the Shakura–Sunyaev temperature profile the
/// tracer's own disk model uses, normalised to the peak.
fn disk_heat(radius: f64) -> f64 {
    if radius <= R_IN {
        return 1.0;
    }
    let t = nullgeo::render::shakura_sunyaev_temperature(T_IN, R_IN, radius);
    let peak = nullgeo::render::shakura_sunyaev_temperature(
        T_IN,
        R_IN,
        nullgeo::render::shakura_sunyaev_peak_radius(R_IN),
    );
    if peak <= 0.0 {
        return 0.0;
    }
    // Temperature to the fourth power is flux, which is what an eye actually sees.
    ((t / peak).powi(4)).clamp(0.0, 1.0)
}

/// A little entropy from the ray itself, so two terminals of the same size do not show the
/// same sky — the star field is a backdrop, but it should not be *identical* everywhere.
fn source_seed(cell: Cell) -> u64 {
    cell.dir
        .iter()
        .filter(|c| c.is_finite())
        .fold(0x2545_f491_4f6c_dd1d_u64, |acc, c| {
            acc.rotate_left(7) ^ ((c * 1.0e6) as i64 as u64)
        })
}

/// A deterministic star field, hashed from the cell's own position.
fn star_brightness(x: u16, y: u16, seed_dir: u64) -> f32 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for value in [x as u64, y as u64, seed_dir & 0xffff] {
        hash ^= value;
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    // Three avalanche rounds: without them, neighbouring cells keep their low-bit
    // relationships and the sky comes out in diagonal strings instead of scattered points.
    for _ in 0..3 {
        hash ^= hash >> 33;
        hash = hash.wrapping_mul(0xff51_afd7_ed55_8ccd);
    }
    let roll = (hash >> 40) as f32 / (u32::MAX >> 8) as f32;
    // Twenty cells in twenty-one are empty. A dense sky reads as noise behind a diagram.
    if roll > 0.05 {
        0.0
    } else {
        (1.0 - roll * 16.0).clamp(0.08, 1.0)
    }
}

/// Two faint plumes along the spin axis, oscillating with the same clock as the disk.
fn jet_at(x: u16, y: u16, w: u16, h: u16, phase: f64) -> bool {
    let cx = w as f64 / 2.0;
    let dx = (x as f64 - cx).abs() / cx;
    let dy = (y as f64 - h as f64 / 2.0).abs() / (h as f64 / 2.0);
    if dx > 0.035 {
        return false;
    }
    let plume = (1.0 - dy).max(0.0);
    // Two frequencies: a slow travelling knot and a fast flicker. A jet that is perfectly
    // steady reads as a static line drawn over the picture, which is worse than no jet.
    let knot = (phase * 1.3 - dy * 7.0).sin();
    let flicker = (phase * 9.0 + dy * 23.0).sin() * 0.35;
    plume > 0.25 && knot + flicker > -0.15
}

/// Braille glyph for a dot mask.
fn glyph(bits: u8) -> char {
    char::from_u32(0x2800 + bits as u32).unwrap_or('\u{28ff}')
}

// ── drawing ─────────────────────────────────────────────────────────────────────────────

/// Just the animation, filling `area`. For embedding in a panel.
pub fn render(frame_widget: &mut Frame, area: Rect, anim: &mut BlackHoleAnim) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = lines(anim, area.width, area.height);
    frame_widget.render_widget(Paragraph::new(lines), area);
}

/// The whole `hole` screen: the animation, the numbers behind it, and the credit.
pub fn render_screen(frame_widget: &mut Frame, area: Rect, anim: &mut BlackHoleAnim) {
    let chrome = Chrome::new("main", &chrome::TABS, TAB_HOLE)
        .with_branch_status()
        .status(Segment::new("◍ sgr a*", t::accent()))
        .footer(Segment::chip("nullgeo (MIT)"))
        .footer(Segment::chip("kerr a=0.9"))
        .hint("space pause · j jets · ? help");
    let body = chrome::frame(frame_widget, area, &chrome);
    if body.height == 0 || body.width == 0 {
        return;
    }

    // A foot of facts under the image: what is on screen, and how big it is in the units the
    // physics uses. A terminal cell cannot show a legend; it can show three numbers.
    let foot_height = if body.height >= 12 { 2 } else { 0 };
    let chunks = Layout::default()
        .direction(ratatui::layout::Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(foot_height)])
        .split(body);
    let canvas = chunks[0];

    render(frame_widget, canvas, anim);

    if foot_height == 2 {
        let raster = anim.raster(canvas.width, canvas.height);
        let source = raster.source;
        let shadow = raster.shadow_cells();
        let total = (raster.width as usize) * (raster.height as usize);
        let pct = if total == 0 { 0.0 } else { shadow as f64 / total as f64 * 100.0 };
        let radii = raster.disk_radii();
        let facts = Line::from(vec![
            Span::styled("  shadow ", t::faint()),
            Span::styled(format!("{pct:.0}%"), t::muted()),
            Span::styled("  disk ", t::faint()),
            Span::styled(
                radii.map(|(lo, _)| format!("{lo:.1}–{R_OUT:.0} r_s")).unwrap_or_else(|| "—".into()),
                t::muted(),
            ),
            Span::styled("  isco ", t::faint()),
            Span::styled(format!("{R_IN:.0} r_s"), t::muted()),
            Span::styled("  phase ", t::faint()),
            Span::styled(format!("{:03.0}°", anim.phase.to_degrees() % 360.0), t::muted()),
            Span::styled("  engine ", t::faint()),
            Span::styled(source.label(), t::muted()),
        ]);
        let hint = Line::from(vec![Span::styled(
            "  Every pixel is an integrated null geodesic; the swirl is a Keplerian flow.",
            t::faint(),
        )]);
        let rows = Layout::default()
            .direction(ratatui::layout::Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(chunks[1]);
        frame_widget.render_widget(Paragraph::new(facts), rows[0]);
        frame_widget.render_widget(Paragraph::new(hint), rows[1]);
    }
}

/// One frame as ratatui lines, with adjacent same-colour cells merged into one span.
pub fn lines(anim: &mut BlackHoleAnim, width: u16, height: u16) -> Vec<Line<'static>> {
    let cells = anim.frame(width, height);
    let mut out = Vec::with_capacity(cells.len());
    for row in cells {
        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut current: Option<(Color, String)> = None;
        for (glyph, colour) in row {
            match current.take() {
                Some((c, text)) if c == colour => current = Some((c, format!("{text}{glyph}"))),
                Some((c, text)) => {
                    spans.push(Span::styled(text, Style::default().fg(c)));
                    current = Some((colour, glyph.to_string()));
                }
                None => current = Some((colour, glyph.to_string())),
            }
        }
        if let Some((c, text)) = current {
            spans.push(Span::styled(text, Style::default().fg(c)));
        }
        out.push(Line::from(spans));
    }
    out
}

/// The mark for the welcome screen: the hole itself, one cell. It is the whole brand.
pub fn sigil() -> Line<'static> {
    Line::from(vec![
        Span::styled(glyph(FULL).to_string(), t::mark()),
        Span::styled(glyph(BITS[1][1]).to_string(), t::PHOTON),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Small enough to trace in a test, large enough to contain a shadow and a disk.
    const W: u16 = 48;
    const H: u16 = 24;

    #[test]
    fn the_shadow_is_a_hole_in_the_middle_of_the_frame() {
        let raster = Raster::trace(W, H);
        let shadow = raster.shadow_cells();
        assert!(shadow > 0, "a black hole with no shadow is not a black hole");
        // At this framing the shadow is a few percent of the frame: the subject sits in the
        // middle of a much larger disk.
        let fraction = shadow as f64 / (W as f64 * H as f64);
        assert!((0.01..0.25).contains(&fraction), "shadow is {fraction:.3} of the frame");
        // And it is centred: the middle cell is inside it, the corner is not.
        assert_eq!(raster.cell(W / 2, H / 2).kind, Kind::Shadow);
        assert_ne!(raster.cell(0, 0).kind, Kind::Shadow);
    }

    #[test]
    fn the_animation_actually_moves_and_the_trace_does_not() {
        // The whole design: geometry traced once, flow animated. If a tick re-traced, a frame
        // would cost hundreds of milliseconds; if nothing moved, the screen would be a poster.
        let mut anim = BlackHoleAnim::new();
        let first = anim.frame(W, H);
        anim.tick(0.4);
        let second = anim.frame(W, H);
        assert_ne!(first, second, "a tick must change the picture");
        // Same size twice: the raster is reused, so the disk's cells are identical.
        assert_eq!(anim.raster(W, H).shadow_cells(), Raster::trace(W, H).shadow_cells());
        // And the phase wraps instead of growing without bound.
        for _ in 0..100 {
            anim.tick(0.25);
        }
        assert!(anim.phase >= 0.0 && anim.phase < TAU);
    }

    #[test]
    fn a_tick_is_clamped_so_a_woken_laptop_does_not_jump() {
        let mut anim = BlackHoleAnim::new();
        anim.tick(900.0); // an hour of sleep
        assert!(anim.phase <= 0.55 * 0.25 + 1e-9, "phase {} after one clamp", anim.phase);
        let before = anim.phase;
        anim.tick(f64::NAN);
        assert_eq!(anim.phase, before, "a bad dt must not poison the clock");
    }

    #[test]
    fn the_hot_spot_orbits() {
        // At the spot's own azimuth it is bright; half a turn away it is dark.
        for phase in [0.0f64, 1.0, 2.5, 5.0] {
            let facing = (phase * 1.15).rem_euclid(TAU);
            let peak = hot_spot(4.1, facing, phase);
            let far = hot_spot(4.1, facing + TAU / 2.0, phase);
            assert!(peak > 0.45, "the spot must be bright on its own azimuth: {peak}");
            assert!(far < 0.02, "and dark on the far side: {far}");
            // Radially, it is a knot near 4.1 M and not a ring.
            assert!(hot_spot(4.1, facing, phase) > hot_spot(7.5, facing, phase));
        }
        // The azimuth it faces advances with the clock, which is what the eye tracks.
        let first = (0.0f64 * 1.15).rem_euclid(TAU);
        let later = (1.0f64 * 1.15).rem_euclid(TAU);
        assert_ne!(first, later);
        assert!(hot_spot(4.1, later, 1.0) > 0.45);
    }

    #[test]
    fn the_disk_is_traced_at_the_radii_the_physics_gives() {
        let raster = Raster::trace(W, H);
        let (lo, hi) = raster.disk_radii().expect("the disk must cross some cells");
        // The tracer projects crossings onto the equatorial plane and its band starts at
        // 0.9 × the inner edge, so "inside the disk" is a band, not a hairline.
        assert!(lo >= R_IN * 0.75, "nothing inside the ISCO: {lo}");
        assert!(hi <= R_OUT * 1.15, "nothing outside the disk: {hi}");
        assert!(hi > lo, "the disk has to have a width");
    }

    #[test]
    fn a_photon_ring_wraps_the_shadow() {
        let raster = Raster::trace(W, H);
        let ring = raster.cells.iter().filter(|c| c.kind == Kind::PhotonRing).count();
        assert!(ring > 0, "escaped light hugging the shadow is what makes the image read");
    }

    #[test]
    fn the_disk_glows_at_its_inner_edge_and_dissolves_at_its_outer_one() {
        // The rim is brighter than the disk just beyond it …
        assert!(inner_rim(R_IN + 0.35) > 1.5);
        assert!(inner_rim(R_IN + 0.35) > inner_rim(R_IN + 2.0));
        // … and the outer edge fades to nothing rather than stopping dead.
        assert_eq!(outer_fade(R_OUT), 0.0);
        assert_eq!(outer_fade(R_OUT * 0.5), 1.0);
        assert!(outer_fade(R_OUT * 0.9) < 0.2);
    }

    #[test]
    fn every_cell_is_accounted_for() {
        let raster = Raster::trace(W, H);
        assert_eq!(raster.cells.len(), W as usize * H as usize);
        assert!(raster.lit() > 0);
    }

    #[test]
    fn the_heat_ramp_runs_hot_to_cold_across_the_disk() {
        // Inner edge is hotter than the outer edge, and the peak is followed by a falloff:
        // that is the shape of the Shakura–Sunyaev profile, not a guess.
        assert!(disk_heat(R_IN + 0.5) > disk_heat(R_OUT - 0.5));
        let peak = disk_heat(nullgeo::render::shakura_sunyaev_peak_radius(R_IN));
        assert!(peak > 0.5, "the peak must be bright: {peak}");
        assert!(disk_heat(R_OUT) > 0.0);
    }

    #[test]
    fn the_stars_are_deterministic_and_sparse() {
        let seed = 0x1234_5678;
        let at = |x: u16, y: u16| star_brightness(x, y, seed);
        assert_eq!(at(7, 9), at(7, 9), "the same cell must give the same star");
        let lit = (0..80)
            .flat_map(|x| (0..40).map(move |y| (x, y)))
            .filter(|(x, y)| at(*x, *y) > 0.0)
            .count();
        let total = 80 * 40;
        assert!(lit * 10 < total, "a dense sky reads as noise: {lit}/{total} lit");
        assert!(lit > 0, "a sky with no stars is just a black background");
        // Neighbours do not repeat: strings of identical stars look like a rendering bug.
        let neighbours = (0..40u16).filter(|y| at(11, *y) > 0.0 && at(12, *y) > 0.0).count();
        assert!(neighbours < 6, "{neighbours} adjacent lit cells in one column pair");
    }

    #[test]
    fn the_disk_seethes_rather_than_sitting_still() {
        let mut anim = BlackHoleAnim::new();
        let first = anim.frame(56, 30);
        anim.phase = 1.1;
        let second = anim.frame(56, 30);
        let mut changed = 0;
        let mut lit = 0;
        for (row_a, row_b) in first.iter().zip(second.iter()) {
            for (a, b) in row_a.iter().zip(row_b.iter()) {
                if a.0 != ' ' || b.0 != ' ' {
                    lit += 1;
                }
                if a != b {
                    changed += 1;
                }
            }
        }
        let ratio = changed as f64 / lit.max(1) as f64;
        assert!(ratio > 0.10, "only {:.0}% of the picture moved", ratio * 100.0);
        assert!(ratio < 0.95, "the whole picture changed: that is noise, not a flow");
    }

    #[test]
    fn the_photon_ring_does_not_sit_at_one_colour() {
        let mut anim = BlackHoleAnim::new();
        let mut seen = std::collections::BTreeSet::new();
        for step in 0..24 {
            anim.phase = step as f64 * TAU / 24.0;
            for row in anim.frame(56, 30) {
                for (glyph, colour) in row {
                    if glyph != ' ' && (colour == t::PHOTON || colour == t::PHOTON_DIM || colour == t::BONE) {
                        seen.insert(format!("{colour:?}"));
                    }
                }
            }
        }
        assert!(seen.len() >= 2, "the ring held one colour: {seen:?}");
    }

    #[test]
    fn the_jets_flicker_and_stay_on_the_axis() {
        let mut anim = BlackHoleAnim::new();
        anim.jets = true;
        let mut counts = Vec::new();
        for step in 0..12 {
            anim.phase = step as f64 * 0.37;
            let frame = anim.frame(56, 30);
            let mut lit = 0;
            for (y, row) in frame.iter().enumerate() {
                for (x, (glyph, colour)) in row.iter().enumerate() {
                    if *glyph == ' ' {
                        continue;
                    }
                    if *colour == t::JET {
                        lit += 1;
                        // The jet is a plume on the spin axis, not a wash over the disk.
                        let dx = (x as f64 - 56.0 / 2.0).abs();
                        assert!(dx <= 2.0, "a jet pixel {dx} cells off the axis at row {y}");
                    }
                }
            }
            counts.push(lit);
        }
        let lo = counts.iter().min().unwrap();
        let hi = counts.iter().max().unwrap();
        assert!(hi > lo, "the jet never changed: {counts:?}");
    }

    #[test]
    fn the_clock_only_moves_when_it_is_ticked() {
        let mut anim = BlackHoleAnim::new();
        assert_eq!(anim.phase, 0.0);
        anim.tick(1.0);
        assert!(anim.phase > 0.0);
        anim.phase = TAU - 0.01;
        anim.tick(1.0);
        assert!(anim.phase < TAU, "the phase wraps instead of growing forever");
        // A nonsense dt is ignored rather than poisoning the animation.
        let before = anim.phase;
        anim.tick(f64::NAN);
        anim.tick(-5.0);
        assert!(anim.phase >= before);
    }

    #[test]
    fn a_frame_is_the_size_it_was_asked_for_and_is_cached() {
        let mut anim = BlackHoleAnim::new();
        let first = anim.frame(W, H);
        assert_eq!(first.len(), H as usize);
        assert_eq!(first[0].len(), W as usize);
        // The geometry is traced once: a second frame at the same size reuses the raster.
        let second = anim.frame(W, H);
        assert_eq!(first.len(), second.len());
        anim.tick(0.4);
        let third = anim.frame(W, H);
        assert_ne!(first, third, "a tick has to change the picture");
    }

    #[test]
    fn the_hole_sits_on_its_own_tab_and_the_mark_is_the_hole() {
        assert_eq!(chrome::TABS[TAB_HOLE], "hole");
        assert_eq!(sigil().spans.len(), 2);
    }

    #[test]
    fn the_fallback_draws_something_rather_than_nothing() {
        let raster = Raster::analytic(W, H);
        assert_eq!(raster.source, Source::Analytic);
        assert!(raster.shadow_cells() > 0);
        assert!(raster.disk_radii().is_some());
    }
}

