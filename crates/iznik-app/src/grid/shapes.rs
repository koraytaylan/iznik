//! Box drawing, block elements and powerline separators, drawn to the cell.
//!
//! Fonts draw these glyphs at their own size and advance, so a frame or a bar
//! chart made of them shows seams and drifts off the grid. Like braille, they
//! are drawn here instead, as rectangles and paths inside the exact cell
//! rectangle text uses, so neighbouring cells always join.

use gpui_kit::{Bounds, Pixels, Point, point, px, size};

/// First code point of the box-drawing block.
const BOX_FIRST: u32 = 0x2500;
/// Last code point of the box-drawing block.
const BOX_LAST: u32 = 0x257F;
/// First code point of the block-elements block.
const BLOCK_FIRST: u32 = 0x2580;
/// Last code point of the block-elements block.
const BLOCK_LAST: u32 = 0x259F;
/// First powerline separator: the solid right-pointing triangle.
const POWERLINE_FIRST: u32 = 0xE0B0;
/// Last powerline separator: the thin left-pointing chevron.
const POWERLINE_LAST: u32 = 0xE0B3;

/// Cell width divided by this is a light line's thickness, before rounding
/// down to a whole logical pixel of at least one.
const LIGHT_DIVISOR: f32 = 8.0;
/// A heavy line is this many light lines thick.
const HEAVY_FACTOR: f32 = 2.0;
/// A double line spans this many light lines: two lines and the gap between.
const DOUBLE_SPAN: f32 = 3.0;
/// A block element is measured in eighths of the cell.
const EIGHTHS: f32 = 8.0;
/// Half of a length: the middle of a cell edge.
const HALF: f32 = 0.5;
/// Part of each dash period that is drawn; the rest is the gap.
const DASH_FILL: f32 = 0.5;
/// Opacity of the light shade, U+2591.
const LIGHT_SHADE: f32 = 0.25;
/// Opacity of the medium shade, U+2592.
const MEDIUM_SHADE: f32 = 0.5;
/// Opacity of the dark shade, U+2593.
const DARK_SHADE: f32 = 0.75;

/// Characters per arm in [`ARMS`]: up, right, down and left.
const ARM_COUNT: usize = 4;
/// Each box-drawing character's arms, four characters apiece in the order up,
/// right, down, left: `.` none, `l` light, `h` heavy, `d` double. Dashes and
/// diagonals are drawn by their own rules and hold `....` here.
const ARMS: &str = concat!(
    ".l.l.h.hl.l.h.h.................", // U+2500
    ".................ll..hl..lh..hh.", // U+2508
    "..ll..lh..hl..hhll..lh..hl..hh..", // U+2510
    "l..ll..hh..lh..hlll.lhl.hll.llh.", // U+2518
    "hlh.hhl.lhh.hhh.l.lll.lhh.lll.hl", // U+2520
    "h.hlh.lhl.hhh.hh.lll.llh.hll.hlh", // U+2528
    ".lhl.lhh.hhl.hhhll.lll.hlh.llh.h", // U+2530
    "hl.lhl.hhh.lhh.hlllllllhlhlllhlh", // U+2538
    "hlllllhlhlhlhllhhhllllhhlhhlhhlh", // U+2540
    "lhhhhlhhhhhlhhhh................", // U+2548
    ".d.dd.d..dl..ld..dd...ld..dl..dd", // U+2550
    "ld..dl..dd..l..dd..ld..dldl.dld.", // U+2558
    "ddd.l.ldd.dld.dd.dld.ldl.dddld.d", // U+2560
    "dl.ldd.dldlddldldddd.ll...lll..l", // U+2568
    "ll.................ll....l....l.", // U+2570
    "...hh....h....h..h.ll.h..l.hh.l.", // U+2578
);

/// Line weight of one arm of a box-drawing character.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Weight {
    /// No arm.
    None,
    /// A thin line.
    Light,
    /// A thick line.
    Heavy,
    /// Two thin lines with a thin gap.
    Double,
}

/// One arm direction, in [`ARMS`] order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Arm {
    /// Toward the top edge.
    Up,
    /// Toward the right edge.
    Right,
    /// Toward the bottom edge.
    Down,
    /// Toward the left edge.
    Left,
}

/// Something to paint inside one cell, in window coordinates.
#[derive(Clone, Debug, PartialEq)]
pub enum CellShape {
    /// A rectangle of the text color at `opacity`.
    Fill {
        /// The rectangle.
        bounds: Bounds<Pixels>,
        /// How much of the text color covers it, from zero to one.
        opacity: f32,
    },
    /// A closed polygon filled with the text color.
    Polygon(Vec<Point<Pixels>>),
    /// An open polyline stroked with the text color.
    Stroke {
        /// The points in order.
        points: Vec<Point<Pixels>>,
        /// The line width.
        width: Pixels,
    },
    /// A quadratic curve stroked with the text color: a rounded corner.
    Curve {
        /// Where the curve starts.
        from: Point<Pixels>,
        /// The control point it bends toward.
        control: Point<Pixels>,
        /// Where the curve ends.
        to: Point<Pixels>,
        /// The line width.
        width: Pixels,
    },
}

/// Whether this character is drawn here rather than by a font.
pub(super) fn is_drawn(character: char) -> bool {
    let code = u32::from(character);
    (BOX_FIRST..=BLOCK_LAST).contains(&code) || (POWERLINE_FIRST..=POWERLINE_LAST).contains(&code)
}

/// What to paint for `character` inside `cell`, or `None` when it is not a
/// character drawn here.
#[must_use]
pub fn cell_shapes(character: char, cell: Bounds<Pixels>) -> Option<Vec<CellShape>> {
    let code = u32::from(character);
    let geometry = Geometry::new(cell);
    if (BOX_FIRST..=BOX_LAST).contains(&code) {
        return Some(geometry.box_drawing(code.checked_sub(BOX_FIRST)?));
    }
    if (BLOCK_FIRST..=BLOCK_LAST).contains(&code) {
        return geometry.block(code.checked_sub(BLOCK_FIRST)?);
    }
    if (POWERLINE_FIRST..=POWERLINE_LAST).contains(&code) {
        return Some(geometry.powerline(code.checked_sub(POWERLINE_FIRST)?));
    }
    None
}

/// The cell being drawn and its line thickness.
struct Geometry {
    /// Left edge.
    left: f32,
    /// Top edge.
    top: f32,
    /// Width of the cell.
    width: f32,
    /// Height of the cell.
    height: f32,
    /// A light line's thickness.
    light: f32,
}

impl Geometry {
    /// Measure `cell` and choose its light line thickness.
    fn new(cell: Bounds<Pixels>) -> Self {
        let width = f32::from(cell.size.width);
        Self {
            left: f32::from(cell.origin.x),
            top: f32::from(cell.origin.y),
            width,
            height: f32::from(cell.size.height),
            light: (width / LIGHT_DIVISOR).floor().max(1.0),
        }
    }

    /// A window point from offsets inside the cell.
    fn at(&self, across: f32, down: f32) -> Point<Pixels> {
        point(px(self.left + across), px(self.top + down))
    }

    /// A filled rectangle from offsets inside the cell.
    fn fill(&self, left: f32, top: f32, right: f32, bottom: f32, opacity: f32) -> CellShape {
        CellShape::Fill {
            bounds: Bounds::new(
                self.at(left, top),
                size(px((right - left).max(0.0)), px((bottom - top).max(0.0))),
            ),
            opacity,
        }
    }

    /// Where a line `thickness` thick starts, centered across `length`.
    fn centered(length: f32, thickness: f32) -> f32 {
        ((length - thickness) * HALF).floor()
    }

    /// The spans an arm of `weight` occupies across a `length`, in order.
    fn lines(&self, weight: Weight, length: f32) -> Vec<(f32, f32)> {
        let thickness = match weight {
            Weight::None => return Vec::new(),
            Weight::Light => self.light,
            Weight::Heavy => self.light * HEAVY_FACTOR,
            Weight::Double => {
                let start = Self::centered(length, self.light * DOUBLE_SPAN);
                let second = start + self.light * HEAVY_FACTOR;
                return vec![(start, start + self.light), (second, second + self.light)];
            }
        };
        let start = Self::centered(length, thickness);
        vec![(start, start + thickness)]
    }

    /// Shapes of one box-drawing character, by its offset in the block.
    fn box_drawing(&self, offset: u32) -> Vec<CellShape> {
        if let Some((count, vertical, heavy)) = dash(offset) {
            return self.dashes(count, vertical, heavy);
        }
        if let Some(diagonals) = diagonal(offset) {
            return diagonals
                .iter()
                .map(|(from, to)| CellShape::Stroke {
                    points: vec![
                        self.at(from.0 * self.width, from.1 * self.height),
                        self.at(to.0 * self.width, to.1 * self.height),
                    ],
                    width: px(self.light),
                })
                .collect();
        }
        let Some(arms) = arms(offset) else {
            return Vec::new();
        };
        if is_arc(offset) {
            return vec![self.arc(arms)];
        }
        [Arm::Up, Arm::Right, Arm::Down, Arm::Left]
            .into_iter()
            .flat_map(|arm| self.arm(arms, arm))
            .collect()
    }

    /// Weight of `arm` in `arms`.
    fn weight(arms: [Weight; ARM_COUNT], arm: Arm) -> Weight {
        arms.get(arm_index(arm)).copied().unwrap_or(Weight::None)
    }

    /// Rectangles of one arm, joined to the arms beside it.
    fn arm(&self, arms: [Weight; ARM_COUNT], arm: Arm) -> Vec<CellShape> {
        let weight = Self::weight(arms, arm);
        let vertical = matches!(arm, Arm::Up | Arm::Down);
        let high = matches!(arm, Arm::Down | Arm::Right);
        let (along, across) = if vertical {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let (negative, positive) = if vertical {
            (Arm::Left, Arm::Right)
        } else {
            (Arm::Up, Arm::Down)
        };
        let side_lines = |side: Arm| self.lines(Self::weight(arms, side), along);
        let structure: Vec<(f32, f32)> = side_lines(negative)
            .into_iter()
            .chain(side_lines(positive))
            .collect();
        let middle = (along * HALF).floor();
        let own = self.lines(weight, across);
        let stops: Vec<f32> = if weight == Weight::Double {
            let opposite = Self::weight(arms, opposite(arm)) != Weight::None;
            [(negative, positive), (positive, negative)]
                .into_iter()
                .map(|(side, other)| {
                    let near = side_lines(side);
                    let far = side_lines(other);
                    let chosen = if high { near.last() } else { near.first() };
                    if let Some(line) = chosen {
                        if high { line.0 } else { line.1 }
                    } else if opposite {
                        middle
                    } else if let (Some(first), Some(last)) = (far.first(), far.last()) {
                        if high { first.0 } else { last.1 }
                    } else {
                        middle
                    }
                })
                .collect()
        } else {
            let lowest = structure.iter().map(|line| line.0).reduce(f32::min);
            let highest = structure.iter().map(|line| line.1).reduce(f32::max);
            let centered = self.lines(weight, along);
            let stop = match (high, lowest, highest) {
                (true, Some(lowest), _) => lowest,
                (false, _, Some(highest)) => highest,
                (true, None, _) => centered.first().map_or(middle, |line| line.0),
                (false, _, None) => centered.first().map_or(middle, |line| line.1),
            };
            vec![stop; own.len()]
        };
        own.iter()
            .zip(stops)
            .map(|(line, stop)| {
                let (start, end) = if high { (stop, along) } else { (0.0, stop) };
                if vertical {
                    self.fill(line.0, start, line.1, end, 1.0)
                } else {
                    self.fill(start, line.0, end, line.1, 1.0)
                }
            })
            .collect()
    }

    /// A rounded corner joining the two light arms of an arc character.
    fn arc(&self, arms: [Weight; ARM_COUNT]) -> CellShape {
        let across = Self::centered(self.width, self.light) + self.light * HALF;
        let down = Self::centered(self.height, self.light) + self.light * HALF;
        let end = |arm: Arm| match arm {
            Arm::Up => self.at(across, 0.0),
            Arm::Right => self.at(self.width, down),
            Arm::Down => self.at(across, self.height),
            Arm::Left => self.at(0.0, down),
        };
        let mut present = [Arm::Up, Arm::Right, Arm::Down, Arm::Left]
            .into_iter()
            .filter(|arm| Self::weight(arms, *arm) != Weight::None);
        let from = present.next().map_or(self.at(across, down), end);
        let to = present.next().map_or(self.at(across, down), end);
        CellShape::Curve {
            from,
            control: self.at(across, down),
            to,
            width: px(self.light),
        }
    }

    /// Evenly spaced dashes along the middle of the cell.
    fn dashes(&self, count: u16, vertical: bool, heavy: bool) -> Vec<CellShape> {
        let weight = if heavy { Weight::Heavy } else { Weight::Light };
        let (along, across) = if vertical {
            (self.height, self.width)
        } else {
            (self.width, self.height)
        };
        let Some(line) = self.lines(weight, across).first().copied() else {
            return Vec::new();
        };
        let period = along / f32::from(count);
        let dash = period * DASH_FILL;
        (0..count)
            .map(|index| {
                let start = period * f32::from(index) + (period - dash) * HALF;
                if vertical {
                    self.fill(line.0, start, line.1, start + dash, 1.0)
                } else {
                    self.fill(start, line.0, start + dash, line.1, 1.0)
                }
            })
            .collect()
    }

    /// Rectangles of one block element, by its offset in the block.
    fn block(&self, offset: u32) -> Option<Vec<CellShape>> {
        let (rectangles, opacity) = block_parts(offset)?;
        let eighth_across = self.width / EIGHTHS;
        let eighth_down = self.height / EIGHTHS;
        Some(
            rectangles
                .iter()
                .map(|(left, top, right, bottom)| {
                    self.fill(
                        f32::from(*left) * eighth_across,
                        f32::from(*top) * eighth_down,
                        f32::from(*right) * eighth_across,
                        f32::from(*bottom) * eighth_down,
                        opacity,
                    )
                })
                .collect(),
        )
    }

    /// One powerline separator: a solid triangle or a thin chevron.
    fn powerline(&self, offset: u32) -> Vec<CellShape> {
        let middle = self.height * HALF;
        let pointing_right = offset < POWERLINE_LEFT;
        let (base, tip) = if pointing_right {
            (0.0, self.width)
        } else {
            (self.width, 0.0)
        };
        let points = vec![
            self.at(base, 0.0),
            self.at(tip, middle),
            self.at(base, self.height),
        ];
        if offset.checked_rem(POWERLINE_KINDS) == Some(0) {
            vec![CellShape::Polygon(points)]
        } else {
            vec![CellShape::Stroke {
                points,
                width: px(self.light),
            }]
        }
    }
}

/// Offset of the first left-pointing powerline separator.
const POWERLINE_LEFT: u32 = 2;
/// Powerline separators come in pairs: solid, then thin.
const POWERLINE_KINDS: u32 = 2;

/// The arm opposite `arm`.
fn opposite(arm: Arm) -> Arm {
    match arm {
        Arm::Up => Arm::Down,
        Arm::Right => Arm::Left,
        Arm::Down => Arm::Up,
        Arm::Left => Arm::Right,
    }
}

/// The position of `arm` in an [`ARMS`] entry.
fn arm_index(arm: Arm) -> usize {
    match arm {
        Arm::Up => 0,
        Arm::Right => 1,
        Arm::Down => ARM_DOWN,
        Arm::Left => ARM_LEFT,
    }
}

/// Position of the down arm in an [`ARMS`] entry.
const ARM_DOWN: usize = 2;
/// Position of the left arm in an [`ARMS`] entry.
const ARM_LEFT: usize = 3;

/// The four arms of the box-drawing character at `offset`.
fn arms(offset: u32) -> Option<[Weight; ARM_COUNT]> {
    let start = usize::try_from(offset).ok()?.checked_mul(ARM_COUNT)?;
    let entry = ARMS.as_bytes().get(start..start.checked_add(ARM_COUNT)?)?;
    let mut arms = [Weight::None; ARM_COUNT];
    for (slot, code) in arms.iter_mut().zip(entry) {
        *slot = match code {
            b'l' => Weight::Light,
            b'h' => Weight::Heavy,
            b'd' => Weight::Double,
            _ => Weight::None,
        };
    }
    Some(arms)
}

/// Offsets of the four rounded corners, U+256D through U+2570.
const ARC_OFFSETS: (u32, u32) = (0x6D, 0x70);

/// Whether the character at `offset` is a rounded corner.
fn is_arc(offset: u32) -> bool {
    (ARC_OFFSETS.0..=ARC_OFFSETS.1).contains(&offset)
}

/// Dashed lines: offset, number of dashes, vertical, heavy.
const DASHES: &[(u32, u16, bool, bool)] = &[
    (0x04, 3, false, false),
    (0x05, 3, false, true),
    (0x06, 3, true, false),
    (0x07, 3, true, true),
    (0x08, 4, false, false),
    (0x09, 4, false, true),
    (0x0A, 4, true, false),
    (0x0B, 4, true, true),
    (0x4C, 2, false, false),
    (0x4D, 2, false, true),
    (0x4E, 2, true, false),
    (0x4F, 2, true, true),
];

/// The dashes of the dashed line at `offset`: count, vertical and heavy.
fn dash(offset: u32) -> Option<(u16, bool, bool)> {
    DASHES
        .iter()
        .find(|entry| entry.0 == offset)
        .map(|entry| (entry.1, entry.2, entry.3))
}

/// A diagonal from one corner to the opposite, as fractions of the cell.
type Diagonal = ((f32, f32), (f32, f32));
/// U+2571, from the upper right to the lower left.
const RISING: Diagonal = ((0.0, 1.0), (1.0, 0.0));
/// U+2572, from the upper left to the lower right.
const FALLING: Diagonal = ((0.0, 0.0), (1.0, 1.0));
/// Offsets of the three diagonal characters.
const DIAGONAL_OFFSETS: (u32, u32, u32) = (0x71, 0x72, 0x73);

/// The diagonals of the character at `offset`, when it is one.
fn diagonal(offset: u32) -> Option<&'static [Diagonal]> {
    const BOTH: &[Diagonal] = &[RISING, FALLING];
    match offset {
        _ if offset == DIAGONAL_OFFSETS.0 => Some(&[RISING]),
        _ if offset == DIAGONAL_OFFSETS.1 => Some(&[FALLING]),
        _ if offset == DIAGONAL_OFFSETS.2 => Some(BOTH),
        _ => None,
    }
}

/// A rectangle in eighths of the cell: left, top, right, bottom.
type Eighths = (u8, u8, u8, u8);
/// Upper left quadrant.
const UPPER_LEFT: Eighths = (0, 0, 4, 4);
/// Upper right quadrant.
const UPPER_RIGHT: Eighths = (4, 0, 8, 4);
/// Lower left quadrant.
const LOWER_LEFT: Eighths = (0, 4, 4, 8);
/// Lower right quadrant.
const LOWER_RIGHT: Eighths = (4, 4, 8, 8);
/// The whole cell.
const WHOLE: Eighths = (0, 0, 8, 8);

/// Each block element, U+2580 onward, as rectangles in eighths.
const BLOCKS: &[&[Eighths]] = &[
    &[(0, 0, 8, 4)],
    &[(0, 7, 8, 8)],
    &[(0, 6, 8, 8)],
    &[(0, 5, 8, 8)],
    &[(0, 4, 8, 8)],
    &[(0, 3, 8, 8)],
    &[(0, 2, 8, 8)],
    &[(0, 1, 8, 8)],
    &[WHOLE],
    &[(0, 0, 7, 8)],
    &[(0, 0, 6, 8)],
    &[(0, 0, 5, 8)],
    &[(0, 0, 4, 8)],
    &[(0, 0, 3, 8)],
    &[(0, 0, 2, 8)],
    &[(0, 0, 1, 8)],
    &[(4, 0, 8, 8)],
    &[WHOLE],
    &[WHOLE],
    &[WHOLE],
    &[(0, 0, 8, 1)],
    &[(7, 0, 8, 8)],
    &[LOWER_LEFT],
    &[LOWER_RIGHT],
    &[UPPER_LEFT],
    &[UPPER_LEFT, LOWER_LEFT, LOWER_RIGHT],
    &[UPPER_LEFT, LOWER_RIGHT],
    &[UPPER_LEFT, UPPER_RIGHT, LOWER_LEFT],
    &[UPPER_LEFT, UPPER_RIGHT, LOWER_RIGHT],
    &[UPPER_RIGHT],
    &[UPPER_RIGHT, LOWER_LEFT],
    &[UPPER_RIGHT, LOWER_LEFT, LOWER_RIGHT],
];

/// Offsets of the three shades, whose one rectangle is partly transparent.
const SHADE_OFFSETS: &[(u32, f32)] = &[
    (0x11, LIGHT_SHADE),
    (0x12, MEDIUM_SHADE),
    (0x13, DARK_SHADE),
];

/// The rectangles and opacity of the block element at `offset`.
fn block_parts(offset: u32) -> Option<(&'static [Eighths], f32)> {
    let rectangles = BLOCKS.get(usize::try_from(offset).ok()?)?;
    let opacity = SHADE_OFFSETS
        .iter()
        .find(|shade| shade.0 == offset)
        .map_or(1.0, |shade| shade.1);
    Some((rectangles, opacity))
}
