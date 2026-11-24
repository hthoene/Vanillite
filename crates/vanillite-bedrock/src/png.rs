//! A small PNG decoder for Java skins: 8-bit RGB or RGBA, not interlaced (what the Mojang skin
//! server delivers). Anything else is refused.

#![allow(clippy::arithmetic_side_effects)] // u8/i16 filter arithmetic and checked sizes

use std::io::Read;

/// Largest decoded image (pixels).
const MAX_PIXELS: usize = 128 * 128;

/// Decodes `png` to RGBA: width, height and pixels.
pub fn decode_rgba(png: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    let body = png.strip_prefix(b"\x89PNG\r\n\x1a\n")?;
    let mut rest = body;
    let mut header: Option<(u32, u32, u8)> = None;
    let mut idat = Vec::new();
    while rest.len() >= 12 {
        let len = usize::try_from(u32::from_be_bytes(rest.get(..4)?.try_into().ok()?)).ok()?;
        let kind = rest.get(4..8)?;
        let data = rest.get(8..8usize.checked_add(len)?)?;
        rest = rest.get(len.checked_add(12)?..)?;
        match kind {
            b"IHDR" => {
                let w = u32::from_be_bytes(data.get(..4)?.try_into().ok()?);
                let h = u32::from_be_bytes(data.get(4..8)?.try_into().ok()?);
                let depth = *data.get(8)?;
                let color = *data.get(9)?;
                let interlace = *data.get(12)?;
                if depth != 8 || interlace != 0 || !matches!(color, 2 | 6) {
                    return None;
                }
                header = Some((w, h, color));
            }
            b"IDAT" => {
                if idat.len().saturating_add(data.len()) > 1 << 20 {
                    return None;
                }
                idat.extend_from_slice(data);
            }
            b"IEND" => break,
            _ => {}
        }
    }
    let (w, h, color) = header?;
    let (wu, hu) = (usize::try_from(w).ok()?, usize::try_from(h).ok()?);
    if wu.checked_mul(hu)? > MAX_PIXELS || wu == 0 || hu == 0 {
        return None;
    }
    let bpp: usize = if color == 6 { 4 } else { 3 };
    let stride = wu.checked_mul(bpp)?;
    let raw_len = stride.checked_add(1)?.checked_mul(hu)?;
    let mut raw = Vec::with_capacity(raw_len);
    flate2::read::ZlibDecoder::new(idat.as_slice())
        .take(u64::try_from(raw_len).ok()?.saturating_add(1))
        .read_to_end(&mut raw)
        .ok()?;
    if raw.len() != raw_len {
        return None;
    }
    let mut pixels = vec![0u8; stride.checked_mul(hu)?];
    let mut prev = vec![0u8; stride];
    for y in 0..hu {
        let line = raw.get(y.checked_mul(stride + 1)?..)?;
        let filter = *line.first()?;
        let src = line.get(1..=stride)?;
        let mut cur = vec![0u8; stride];
        for x in 0..stride {
            let a = if x >= bpp { *cur.get(x - bpp)? } else { 0 };
            let b = *prev.get(x)?;
            let c = if x >= bpp { *prev.get(x - bpp)? } else { 0 };
            let v = *src.get(x)?;
            let p = match filter {
                0 => 0,
                1 => a,
                2 => b,
                3 => u8::try_from((u16::from(a) + u16::from(b)) / 2).ok()?,
                4 => paeth(a, b, c),
                _ => return None,
            };
            *cur.get_mut(x)? = v.wrapping_add(p);
        }
        pixels
            .get_mut(y.checked_mul(stride)?..y.checked_mul(stride)?.checked_add(stride)?)?
            .copy_from_slice(&cur);
        prev = cur;
    }
    if color == 2 {
        let rgba = pixels
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 0xff])
            .collect();
        return Some((w, h, rgba));
    }
    Some((w, h, pixels))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (ia, ib, ic) = (i16::from(a), i16::from(b), i16::from(c));
    let p = ia + ib - ic;
    let (pa, pb, pc) = ((p - ia).abs(), (p - ib).abs(), (p - ic).abs());
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn decodes_a_small_rgba_image() {
        // 2x1 RGBA, filter "sub" on the only line.
        let raw = [1u8, 10, 20, 30, 255, 5, 5, 5, 0];
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        let _ = z.write_all(&raw);
        let idat = z.finish().unwrap_or_default();
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut chunk = |kind: &[u8], data: &[u8]| {
            png.extend_from_slice(&u32::try_from(data.len()).unwrap_or(0).to_be_bytes());
            png.extend_from_slice(kind);
            png.extend_from_slice(data);
            png.extend_from_slice(&[0; 4]);
        };
        chunk(b"IHDR", &[0, 0, 0, 2, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        chunk(b"IDAT", &idat);
        chunk(b"IEND", &[]);
        let (w, h, px) = decode_rgba(&png).unwrap_or_default();
        assert_eq!((w, h), (2, 1));
        assert_eq!(px, vec![10, 20, 30, 255, 15, 25, 35, 255]);
    }
}
