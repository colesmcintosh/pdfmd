//! Minimal PNG encoder for decoded 8-bit raster image XObjects.

pub(super) fn encode_png(width: usize, height: usize, color_type: u8, pixels: &[u8]) -> Vec<u8> {
    let channels = if color_type == 0 { 1 } else { 3 };
    let row_len = width * channels;
    let scanline_len = height * (row_len + 1);
    let mut out = Vec::with_capacity(scanline_len + 128);
    let crc_table = crc32_table();
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = Vec::with_capacity(13);
    ihdr.extend_from_slice(&(width as u32).to_be_bytes());
    ihdr.extend_from_slice(&(height as u32).to_be_bytes());
    ihdr.push(8); // bit depth
    ihdr.push(color_type);
    ihdr.extend_from_slice(&[0, 0, 0]); // compression, filter, interlace
    write_png_chunk(&mut out, b"IHDR", &ihdr, &crc_table);
    write_png_idat(&mut out, row_len, height, pixels, &crc_table);
    write_png_chunk(&mut out, b"IEND", &[], &crc_table);
    out
}

fn write_png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8], table: &[u32; 256]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    out.extend_from_slice(&crc32_pair(kind, data, table).to_be_bytes());
}

fn write_png_idat(
    out: &mut Vec<u8>,
    row_len: usize,
    height: usize,
    pixels: &[u8],
    table: &[u32; 256],
) {
    let scanline_len = height * (row_len + 1);
    let block_count = if scanline_len == 0 {
        1
    } else {
        (scanline_len + 65_534) / 65_535
    };
    let data_len = 2 + block_count * 5 + scanline_len + 4;
    out.extend_from_slice(&(data_len as u32).to_be_bytes());
    out.extend_from_slice(b"IDAT");
    let data_start = out.len();
    out.extend_from_slice(&[0x78, 0x01]); // deflate, 32K window, fastest algorithm

    let mut pos = 0usize;
    let mut rest = scanline_len;
    let mut adler = (1u32, 0u32);
    while rest > 0 {
        let take = rest.min(65_535);
        let final_block = take == rest;
        write_stored_block_header(out, take, final_block);
        append_png_scanline_bytes(out, pixels, row_len, &mut pos, take, &mut adler);
        rest -= take;
    }
    if scanline_len == 0 {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }

    out.extend_from_slice(&((adler.1 << 16) | adler.0).to_be_bytes());
    let crc = crc32_pair(b"IDAT", &out[data_start..], table);
    out.extend_from_slice(&crc.to_be_bytes());
}

fn write_stored_block_header(out: &mut Vec<u8>, len: usize, final_block: bool) {
    out.push(if final_block { 1 } else { 0 });
    let len = len as u16;
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&(!len).to_le_bytes());
}

fn append_png_scanline_bytes(
    out: &mut Vec<u8>,
    pixels: &[u8],
    row_len: usize,
    pos: &mut usize,
    mut len: usize,
    adler: &mut (u32, u32),
) {
    let stride = row_len + 1;
    while len > 0 {
        let col = *pos % stride;
        if col == 0 {
            out.push(0);
            adler_push(adler, 0);
            *pos += 1;
            len -= 1;
            continue;
        }

        let row = *pos / stride;
        let src_offset = row * row_len + col - 1;
        let take = (row_len - (col - 1)).min(len);
        let bytes = &pixels[src_offset..src_offset + take];
        out.extend_from_slice(bytes);
        adler_update(adler, bytes);
        *pos += take;
        len -= take;
    }
}

#[cfg(test)]
pub(super) fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + (data.len() / 65_535 + 1) * 5 + 6);
    out.extend_from_slice(&[0x78, 0x01]); // deflate, 32K window, fastest algorithm
    let mut rest = data;
    while !rest.is_empty() {
        let take = rest.len().min(65_535);
        let final_block = take == rest.len();
        out.push(if final_block { 1 } else { 0 });
        let len = take as u16;
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(&rest[..take]);
        rest = &rest[take..];
    }
    if data.is_empty() {
        out.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

#[cfg(test)]
fn adler32(data: &[u8]) -> u32 {
    let mut adler = (1u32, 0u32);
    adler_update(&mut adler, data);
    (adler.1 << 16) | adler.0
}

fn adler_push(adler: &mut (u32, u32), byte: u8) {
    const MOD: u32 = 65_521;
    adler.0 += byte as u32;
    adler.1 += adler.0;
    if adler.1 >= MOD {
        adler.0 %= MOD;
        adler.1 %= MOD;
    }
}

fn adler_update(adler: &mut (u32, u32), data: &[u8]) {
    const MOD: u32 = 65_521;
    const NMAX: usize = 5_552;
    for chunk in data.chunks(NMAX) {
        for &byte in chunk {
            adler.0 += byte as u32;
            adler.1 += adler.0;
        }
        adler.0 %= MOD;
        adler.1 %= MOD;
    }
}

fn crc32_pair(a: &[u8], b: &[u8], table: &[u32; 256]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    crc = crc32_update(crc, a, table);
    crc = crc32_update(crc, b, table);
    !crc
}

fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut crc = i as u32;
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
        }
        *slot = crc;
    }
    table
}

fn crc32_update(mut crc: u32, data: &[u8], table: &[u32; 256]) -> u32 {
    for &byte in data {
        crc = (crc >> 8) ^ table[((crc ^ byte as u32) & 0xFF) as usize];
    }
    crc
}
