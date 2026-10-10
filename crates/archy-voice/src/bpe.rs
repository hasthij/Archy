//! Turning a phrase into the keyword model's tokens ("CHILD" -> "▁CHI L D"),
//! the way SentencePiece does for this (unigram) model.

use std::collections::HashMap;

/// Piece -> score, read from the model's `bpe.model` (a SentencePiece protobuf).
pub struct Vocab {
    scores: HashMap<String, f32>,
}

fn varint(b: &[u8], i: &mut usize) -> Option<u64> {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        v |= ((c & 0x7f) as u64) << shift;
        if c < 0x80 {
            return Some(v);
        }
        shift += 7;
    }
}

/// Skips one protobuf field of wire type `wire` at `i`.
fn skip(b: &[u8], i: &mut usize, wire: u64) -> Option<()> {
    match wire {
        0 => {
            varint(b, i)?;
        }
        1 => *i += 8,
        2 => {
            let n = varint(b, i)? as usize;
            *i += n;
        }
        5 => *i += 4,
        _ => return None,
    }
    Some(())
}

impl Vocab {
    /// Parses `ModelProto.pieces` (field 1): each `SentencePiece` has piece (1) and score (2).
    pub fn from_model(bytes: &[u8]) -> Option<Self> {
        let mut scores = HashMap::new();
        let mut i = 0;
        while i < bytes.len() {
            let key = varint(bytes, &mut i)?;
            let (field, wire) = (key >> 3, key & 7);
            if field == 1 && wire == 2 {
                let n = varint(bytes, &mut i)? as usize;
                let msg = bytes.get(i..i + n)?;
                i += n;
                let (mut j, mut piece, mut score) = (0, None, 0f32);
                while j < msg.len() {
                    let k = varint(msg, &mut j)?;
                    match (k >> 3, k & 7) {
                        (1, 2) => {
                            let l = varint(msg, &mut j)? as usize;
                            piece = String::from_utf8(msg.get(j..j + l)?.to_vec()).ok();
                            j += l;
                        }
                        (2, 5) => {
                            score = f32::from_le_bytes(msg.get(j..j + 4)?.try_into().ok()?);
                            j += 4;
                        }
                        (_, w) => skip(msg, &mut j, w)?,
                    }
                }
                if let Some(p) = piece {
                    scores.insert(p, score);
                }
            } else {
                skip(bytes, &mut i, wire)?;
            }
        }
        (!scores.is_empty()).then_some(Self { scores })
    }

    /// Space-separated tokens for `phrase` (uppercased). The model is a SentencePiece
    /// *unigram* model (it has "▁CHI" but no "▁CH", impossible for BPE), so each word
    /// is split the unigram way: the segmentation with the highest total score
    /// (Viterbi). Matches SentencePiece's own output, e.g. "CHILD" -> "▁CHI L D".
    pub fn encode(&self, phrase: &str) -> String {
        let mut out: Vec<String> = Vec::new();
        for word in phrase.split_whitespace() {
            let word: String = word
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .collect::<String>()
                .to_uppercase();
            if word.is_empty() {
                continue;
            }
            let chars: Vec<char> = std::iter::once('\u{2581}').chain(word.chars()).collect();
            let n = chars.len();
            // best[i] = (score, start of last piece) for the prefix of length i.
            let mut best: Vec<Option<(f32, usize)>> = vec![None; n + 1];
            best[0] = Some((0.0, 0));
            for end in 1..=n {
                for start in 0..end {
                    let Some((prefix, _)) = best[start] else { continue };
                    let piece: String = chars[start..end].iter().collect();
                    let score = match self.scores.get(&piece) {
                        Some(&s) => s,
                        // Unknown single character: allowed, heavily penalized.
                        None if end - start == 1 => -20.0,
                        None => continue,
                    };
                    let total = prefix + score;
                    if best[end].is_none_or(|(b, _)| total > b) {
                        best[end] = Some((total, start));
                    }
                }
            }
            let mut pieces = Vec::new();
            let mut end = n;
            while end > 0 {
                let (_, start) = best[end].expect("every prefix is reachable");
                pieces.push(chars[start..end].iter().collect::<String>());
                end = start;
            }
            pieces.reverse();
            out.extend(pieces);
        }
        out.join(" ")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn viterbi_prefers_higher_total_score() {
        let mut v = super::Vocab { scores: Default::default() };
        for (p, s) in [
            ("\u{2581}", -4.6),
            ("\u{2581}C", -5.2),
            ("\u{2581}CHI", -7.5),
            ("C", -4.7),
            ("H", -6.2),
            ("I", -5.2),
            ("IL", -6.0),
            ("L", -5.0),
            ("D", -4.2),
        ] {
            v.scores.insert(p.to_string(), s);
        }
        assert_eq!(v.encode("child"), "\u{2581}CHI L D");
    }
}
