//! The mouse cursor on the bare screen: its images, and drawing them.
//!
//! There is no desktop to draw a cursor for us, so Shine has two ways:
//!
//! - a **hardware cursor**: the display controller lays a small image over
//!   the screen and moves it by itself, so moving the mouse costs nothing
//!   and needs no new frame. Intel's i915 has one (the netbook's GMA 3150).
//! - **drawn into the frame** when the driver has none (QEMU's bochs): the
//!   image is painted over each frame as it goes to the screen, and a
//!   moved mouse means a new frame.
//!
//! The images are written out as text below: `#` is the outline, `.` the
//! fill, and a space is transparent.

use minifb::CursorStyle;

pub(super) struct Shape {
    rows: &'static [&'static str],
    /// The pixel that is the pointer's position.
    pub hotspot: (i32, i32),
}

const ARROW: Shape = Shape {
    rows: &[
        "#",
        "##",
        "#.#",
        "#..#",
        "#...#",
        "#....#",
        "#.....#",
        "#......#",
        "#.......#",
        "#........#",
        "#.........#",
        "#......#####",
        "#...#..#",
        "#..# #..#",
        "#.#  #..#",
        "##    #..#",
        "#     #..#",
        "       ##",
    ],
    hotspot: (0, 0),
};

const IBEAM: Shape = Shape {
    rows: &[
        "### ###", "#..#..#", " ##.## ", "  #.#  ", "  #.#  ", "  #.#  ", "  #.#  ", "  #.#  ",
        "  #.#  ", "  #.#  ", "  #.#  ", "  #.#  ", "  #.#  ", " ##.## ", "#..#..#", "### ###",
    ],
    hotspot: (3, 8),
};

const CROSSHAIR: Shape = Shape {
    rows: &[
        "    ###    ",
        "    #.#    ",
        "    #.#    ",
        "    #.#    ",
        "#####.#####",
        "#.........#",
        "#####.#####",
        "    #.#    ",
        "    #.#    ",
        "    #.#    ",
        "    ###    ",
    ],
    hotspot: (5, 5),
};

/// The image for a cursor style. Styles without an image of their own (the
/// hands and resize arrows) use the arrow for now.
pub(super) fn shape(style: CursorStyle) -> &'static Shape {
    match style {
        CursorStyle::Ibeam => &IBEAM,
        CursorStyle::Crosshair => &CROSSHAIR,
        _ => &ARROW,
    }
}

const OUTLINE: u32 = 0x000000;
const FILL: u32 = 0xFFFFFF;

impl Shape {
    /// Every opaque pixel as `(x, y, colour)`, relative to the hotspot.
    fn pixels(&self) -> impl Iterator<Item = (i32, i32, u32)> + '_ {
        self.rows.iter().enumerate().flat_map(move |(y, row)| {
            row.bytes().enumerate().filter_map(move |(x, b)| {
                let colour = match b {
                    b'#' => OUTLINE,
                    b'.' => FILL,
                    _ => return None,
                };
                Some((x as i32 - self.hotspot.0, y as i32 - self.hotspot.1, colour))
            })
        })
    }

    /// Paint the cursor into a 32-bit frame `pitch` bytes per row and
    /// `w`×`h` pixels, with its hotspot at `(x, y)`.
    pub(super) fn draw(&self, frame: &mut [u8], pitch: usize, w: usize, h: usize, x: i32, y: i32) {
        for (dx, dy, colour) in self.pixels() {
            let (px, py) = (x + dx, y + dy);
            if px < 0 || py < 0 || px as usize >= w || py as usize >= h {
                continue;
            }
            let at = py as usize * pitch + px as usize * 4;
            frame[at..at + 4].copy_from_slice(&colour.to_le_bytes());
        }
    }

    /// The cursor as a `size`×`size` ARGB image for a hardware cursor, with
    /// the hotspot at the top-left corner (`(0, 0)`) — so the image goes at
    /// the pointer position minus [`Shape::hotspot`].
    pub(super) fn argb(&self, size: usize) -> Vec<u32> {
        let mut image = vec![0u32; size * size];
        for (dx, dy, colour) in self.pixels() {
            let (px, py) = (dx + self.hotspot.0, dy + self.hotspot.1);
            if (px as usize) < size && (py as usize) < size {
                image[py as usize * size + px as usize] = 0xFF00_0000 | colour;
            }
        }
        image
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hotspot_pixel_is_drawn_at_the_pointer() {
        let (w, h) = (32, 32);
        let mut frame = vec![0xAAu8; w * h * 4];
        ARROW.draw(&mut frame, w * 4, w, h, 10, 10);
        let at = (10 * w + 10) * 4;
        assert_eq!(&frame[at..at + 4], &OUTLINE.to_le_bytes());
        // Left of the arrow's tip is untouched.
        let left = (10 * w + 9) * 4;
        assert_eq!(&frame[left..left + 4], &[0xAA; 4]);
    }

    #[test]
    fn a_cursor_at_the_edge_is_clipped_not_wrapped() {
        let (w, h) = (8, 8);
        let mut frame = vec![0u8; w * h * 4];
        ARROW.draw(&mut frame, w * 4, w, h, 6, 6);
        ARROW.draw(&mut frame, w * 4, w, h, -3, -3);
    }

    #[test]
    fn the_hardware_image_is_opaque_only_where_the_shape_is() {
        let image = IBEAM.argb(64);
        assert_eq!(image[0] >> 24, 0xFF);
        assert_eq!(image[3] >> 24, 0x00); // the gap in "### ###"
        assert_eq!(image[63 * 64 + 63], 0);
    }
}
