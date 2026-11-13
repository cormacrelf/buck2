// Hillshading: light falling on a heightmap.
//
// JavaScript puts a heightmap into this module's memory with `alloc`, calls `shade`, and reads
// back RGBA pixels: each cell lit by its slope and aspect, warm where it faces the sun and a cool
// grey where it turns away.

/// Where the light comes from, in degrees clockwise from north (up the heightmap).
const AZIMUTH: f32 = 315.0;
/// How high the light is, in degrees above the horizon.
const ALTITUDE: f32 = 45.0;
/// Heights are multiplied by this, so that low relief still casts shade.
const Z_FACTOR: f32 = 40.0;

/// The colour of a cell in full shade, and in full light.
const SHADOW: [f32; 3] = [52.0, 58.0, 74.0];
const LIGHT: [f32; 3] = [255.0, 239.0, 205.0];

/// `bytes` of memory, aligned for f32, for JavaScript to fill. Free it with `dealloc`.
#[unsafe(no_mangle)]
pub extern "C" fn alloc(bytes: usize) -> *mut u8 {
    match layout(bytes) {
        Some(layout) => unsafe { std::alloc::alloc(layout) },
        None => std::ptr::null_mut(),
    }
}

/// Frees memory from `alloc(bytes)`.
///
/// # Safety
/// `ptr` came from `alloc` with the same `bytes`, and is not used afterwards.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dealloc(ptr: *mut u8, bytes: usize) {
    if let Some(layout) = layout(bytes) {
        unsafe { std::alloc::dealloc(ptr, layout) }
    }
}

fn layout(bytes: usize) -> Option<std::alloc::Layout> {
    std::alloc::Layout::from_size_align(bytes.max(1), 8).ok()
}

/// Writes `width * height` RGBA pixels to `out`, shading the `width * height` heights, row by row.
///
/// # Safety
/// `heights` points to `width * height` f32s and `out` to `4 * width * height` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn shade(heights: *const f32, width: u32, height: u32, out: *mut u8) {
    let (w, h) = (width as usize, height as usize);
    let heights = unsafe { std::slice::from_raw_parts(heights, w * h) };
    let out = unsafe { std::slice::from_raw_parts_mut(out, 4 * w * h) };
    shade_into(heights, w, h, out);
}

fn shade_into(heights: &[f32], w: usize, h: usize, out: &mut [u8]) {
    let zenith = (90.0 - ALTITUDE).to_radians();
    // The azimuth as an angle anticlockwise from east, as the aspect is measured below.
    let azimuth = (360.0 - AZIMUTH + 90.0).rem_euclid(360.0).to_radians();

    // A height, with the edges repeated outwards.
    let at = |x: isize, y: isize| -> f32 {
        let x = x.clamp(0, w as isize - 1) as usize;
        let y = y.clamp(0, h as isize - 1) as usize;
        heights[y * w + x] * Z_FACTOR
    };

    for y in 0..h {
        for x in 0..w {
            let (xi, yi) = (x as isize, y as isize);
            // Horn's method: the gradient from the eight neighbours, the nearer ones weighted twice.
            let (a, b, c) = (at(xi - 1, yi - 1), at(xi, yi - 1), at(xi + 1, yi - 1));
            let (d, f) = (at(xi - 1, yi), at(xi + 1, yi));
            let (g, hh, i) = (at(xi - 1, yi + 1), at(xi, yi + 1), at(xi + 1, yi + 1));
            let dzdx = ((c + 2.0 * f + i) - (a + 2.0 * d + g)) / 8.0;
            let dzdy = ((g + 2.0 * hh + i) - (a + 2.0 * b + c)) / 8.0;

            let slope = dzdx.hypot(dzdy).atan();
            // Rows run southwards, so north is -y.
            let aspect = dzdy.atan2(-dzdx);

            let lit = zenith.cos() * slope.cos() + zenith.sin() * slope.sin() * (azimuth - aspect).cos();
            let lit = lit.clamp(0.0, 1.0);

            let px = &mut out[4 * (y * w + x)..][..4];
            for (channel, (s, l)) in px.iter_mut().zip(SHADOW.iter().zip(LIGHT)) {
                *channel = (s + (l - s) * lit).round() as u8;
            }
            px[3] = 255;
        }
    }
}

fn main() {
    println!("hillshade: shaded relief of a heightmap, lit from azimuth {AZIMUTH}° at altitude {ALTITUDE}°");
}
