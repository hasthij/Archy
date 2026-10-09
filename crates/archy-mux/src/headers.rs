//! Codec configuration records extracted from a keyframe, as the MP4 sink
//! expects them in `MF_MT_MPEG_SEQUENCE_HEADER`.

use archy_gpu::probe::Codec;

/// H.264 / HEVC: parameter-set NAL units with Annex B start codes.
/// AV1: an `av1C` record (4-byte header + the sequence header OBU).
pub fn codec_header(codec: Codec, keyframe: &[u8]) -> Option<Vec<u8>> {
    match codec {
        Codec::H264 => parameter_sets(keyframe, |nal| matches!(nal[0] & 0x1f, 7 | 8)),
        Codec::Hevc => parameter_sets(keyframe, |nal| matches!((nal[0] >> 1) & 0x3f, 32..=34)),
        Codec::Av1 => av1c(keyframe),
    }
}

/// Annex B NAL units (without start codes).
fn nal_units(data: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let ends: Vec<usize> = starts
        .iter()
        .skip(1)
        .map(|&s| {
            // Back off the next start code, including a 4-byte one's leading zero.
            let mut e = s - 3;
            while e > 0 && data[e - 1] == 0 {
                e -= 1;
            }
            e
        })
        .chain(std::iter::once(data.len()))
        .collect();
    starts
        .into_iter()
        .zip(ends)
        .filter(|(s, e)| e > s)
        .map(move |(s, e)| &data[s..e])
}

fn parameter_sets(keyframe: &[u8], wanted: impl Fn(&[u8]) -> bool) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for nal in nal_units(keyframe).filter(|n| !n.is_empty() && wanted(n)) {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    (!out.is_empty()).then_some(out)
}

const OBU_SEQUENCE_HEADER: u8 = 1;

fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value = 0u64;
    for (i, &b) in data.iter().take(8).enumerate() {
        value |= ((b & 0x7f) as u64) << (7 * i);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}

/// Finds the sequence header OBU (whole OBU, header included) in a temporal unit.
fn sequence_header_obu(data: &[u8]) -> Option<(&[u8], &[u8])> {
    let mut pos = 0;
    while pos < data.len() {
        let header = data[pos];
        let obu_type = (header >> 3) & 0x0f;
        let has_ext = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        let mut p = pos + 1 + has_ext as usize;
        let size = if has_size {
            let (size, n) = leb128(data.get(p..)?)?;
            p += n;
            size as usize
        } else {
            data.len() - p
        };
        let end = p.checked_add(size)?;
        if end > data.len() {
            return None;
        }
        if obu_type == OBU_SEQUENCE_HEADER {
            return Some((&data[pos..end], &data[p..end]));
        }
        pos = end;
    }
    None
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn read(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = *self.data.get(self.pos / 8)?;
            v = (v << 1) | ((byte >> (7 - self.pos % 8)) & 1) as u32;
            self.pos += 1;
        }
        Some(v)
    }

    fn uvlc(&mut self) -> Option<u32> {
        let mut zeros = 0;
        while self.read(1)? == 0 {
            zeros += 1;
            if zeros >= 32 {
                return None;
            }
        }
        Some(self.read(zeros)? + ((1u32 << zeros) - 1))
    }
}

/// `av1C` (AV1 Codec ISO Media File Format Binding §2.3) for 8-bit 4:2:0 streams,
/// which is everything Archy records.
fn av1c(keyframe: &[u8]) -> Option<Vec<u8>> {
    let (obu, payload) = sequence_header_obu(keyframe)?;
    let mut b = Bits {
        data: payload,
        pos: 0,
    };
    let profile = b.read(3)?;
    let _still_picture = b.read(1)?;
    let reduced = b.read(1)? == 1;
    let (level, tier) = if reduced {
        (b.read(5)?, 0)
    } else {
        if b.read(1)? == 1 {
            // timing_info
            b.read(32)?;
            b.read(32)?;
            if b.read(1)? == 1 {
                b.uvlc()?;
            }
            // decoder_model_info
            if b.read(1)? == 1 {
                b.read(5 + 32 + 5 + 5)?;
            }
        }
        let _initial_display_delay_present = b.read(1)?;
        let _op_count_minus_1 = b.read(5)?;
        let _idc = b.read(12)?;
        let level = b.read(5)?;
        let tier = if level > 7 { b.read(1)? } else { 0 };
        (level, tier)
    };
    let mut out = vec![
        0x81, // marker + version 1
        ((profile << 5) | level) as u8,
        // tier, high_bitdepth 0, twelve_bit 0, monochrome 0, subsampling_x 1, subsampling_y 1, position 0
        ((tier << 7) | 0b0000_1100) as u8,
        0, // no initial presentation delay
    ];
    out.extend_from_slice(obu);
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_parameter_sets_are_extracted_with_start_codes() {
        let frame = [
            0, 0, 0, 1, 0x67, 1, 2, // SPS
            0, 0, 1, 0x68, 3, // PPS
            0, 0, 0, 1, 0x65, 9, 9, 9, // IDR slice
        ];
        let header = codec_header(Codec::H264, &frame).unwrap();
        assert_eq!(header, [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 0, 1, 0x68, 3]);
    }

    #[test]
    fn hevc_parameter_sets_are_extracted() {
        let frame = [
            0, 0, 1, 0x40, 1, 0xAA, 0, 0, 1, 0x42, 1, 0xBB, 0, 0, 1, 0x44, 1, 0xCC, 0, 0, 1, 0x26,
            1, 0xDD,
        ];
        let header = codec_header(Codec::Hevc, &frame).unwrap();
        assert_eq!(header.len(), 3 * (4 + 3));
        assert_eq!(&header[4..7], &[0x40, 1, 0xAA]);
    }

    #[test]
    fn av1c_from_reduced_sequence_header() {
        // Temporal delimiter OBU, then a sequence header OBU whose payload is
        // profile 0, still 0, reduced 1, level 8 (0b01000) -> bits 000 0 1 01000.
        let frame = [0x12, 0x00, 0x0A, 0x02, 0b0000_1010, 0b0000_0000];
        let c = codec_header(Codec::Av1, &frame).unwrap();
        assert_eq!(c[0], 0x81);
        assert_eq!(c[1], 8); // profile 0, level 8
        assert_eq!(&c[4..], &[0x0A, 0x02, 0b0000_1010, 0]);
    }
}
