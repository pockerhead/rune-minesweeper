//! A run code: a won game written down move by move, compact enough for a URL.
//!
//! Bytes, then base64url without padding:
//!
//! ```text
//! format      u8        FORMAT
//! level       u8        index into LEVELS
//! version     3 varints the game's major, minor, patch
//! seed        u64 LE    the board's seed
//! fingerprint u32 LE    Board::fingerprint once the mines were laid
//! moves       varint    how many, then per move:
//!   what      varint    cell << 1 | 1 for a flag toggle, 0 for an open
//!   delta     varint    milliseconds since the move before (the first move's is 0)
//! checksum    u32 LE    FNV-1a of every byte before it
//! ```
//!
//! Varints are LEB128. The checksum only catches a code mangled on the way (a cut URL);
//! anyone can recompute it, so honesty is the replay's job (`replay`).

/// The run code layout this build writes and reads.
pub const FORMAT: u8 = 1;
/// The longest run code read, in characters: well over a long expert game (see the tests).
pub const MAX_CODE: usize = 8000;
/// The most moves a run may have.
pub const MAX_MOVES: usize = 5000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Open,
    Flag,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Move {
    pub kind: Kind,
    pub cell: usize,
    /// Milliseconds since the first move.
    pub ms: u32,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Run {
    pub level: u8,
    pub version: [u32; 3],
    pub seed: u64,
    pub fingerprint: u32,
    pub moves: Vec<Move>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CodeError {
    /// Not base64url, too long, cut short, or with bytes left over.
    Malformed,
    /// The checksum doesn't match: the code was changed or cut on the way.
    Checksum,
    /// A layout this build doesn't read.
    Format,
}

impl std::fmt::Display for CodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CodeError::Malformed => "the run code is not a valid code (cut short or mistyped?)",
            CodeError::Checksum => "the run code's checksum does not match (it was changed or cut short)",
            CodeError::Format => "the run code is from a version of the game this leaderboard does not read",
        })
    }
}

/// The game's version as written in a run code, from "major.minor.patch".
pub fn parse_version(v: &str) -> [u32; 3] {
    let mut parts = v.split('.').map(|p| p.parse().unwrap_or(0));
    [parts.next().unwrap_or(0), parts.next().unwrap_or(0), parts.next().unwrap_or(0)]
}

impl Run {
    /// A run from moves timed in seconds on any clock: the times count from the first move,
    /// in whole milliseconds, each at least one after the one before (two moves in one frame
    /// share a clock reading).
    pub fn from_times(level: u8, version: [u32; 3], seed: u64, fingerprint: u32, moves: &[(Kind, usize, f64)]) -> Run {
        let t0 = moves.first().map_or(0.0, |m| m.2);
        let mut last: Option<u32> = None;
        let moves = moves
            .iter()
            .map(|&(kind, cell, t)| {
                let ms = ((t - t0) * 1000.0).round().max(0.0) as u32;
                let ms = last.map_or(ms, |l| ms.max(l + 1));
                last = Some(ms);
                Move { kind, cell, ms }
            })
            .collect();
        Run { level, version, seed, fingerprint, moves }
    }

    /// The run's time: its last move's.
    pub fn time_ms(&self) -> u32 {
        self.moves.last().map_or(0, |m| m.ms)
    }

    pub fn version_text(&self) -> String {
        format!("{}.{}.{}", self.version[0], self.version[1], self.version[2])
    }

    pub fn encode(&self) -> String {
        let mut b = vec![FORMAT, self.level];
        for v in self.version {
            put_varint(&mut b, v as u64);
        }
        b.extend_from_slice(&self.seed.to_le_bytes());
        b.extend_from_slice(&self.fingerprint.to_le_bytes());
        put_varint(&mut b, self.moves.len() as u64);
        let mut prev = 0;
        for m in &self.moves {
            put_varint(&mut b, (m.cell as u64) << 1 | (m.kind == Kind::Flag) as u64);
            put_varint(&mut b, m.ms.saturating_sub(prev) as u64);
            prev = m.ms;
        }
        let sum = fnv(&b);
        b.extend_from_slice(&sum.to_le_bytes());
        base64url(&b)
    }

    pub fn decode(code: &str) -> Result<Run, CodeError> {
        let code = code.trim();
        if code.is_empty() || code.len() > MAX_CODE {
            return Err(CodeError::Malformed);
        }
        let b = unbase64url(code).ok_or(CodeError::Malformed)?;
        if b.len() < 4 + 2 {
            return Err(CodeError::Malformed);
        }
        let (body, sum) = b.split_at(b.len() - 4);
        if fnv(body).to_le_bytes() != sum {
            return Err(CodeError::Checksum);
        }
        if body[0] != FORMAT {
            return Err(CodeError::Format);
        }
        let mut r = Reader { b: body, at: 1 };
        let level = r.byte()?;
        let version = [r.u32()?, r.u32()?, r.u32()?];
        let seed = u64::from_le_bytes(r.take(8)?.try_into().map_err(|_| CodeError::Malformed)?);
        let fingerprint = u32::from_le_bytes(r.take(4)?.try_into().map_err(|_| CodeError::Malformed)?);
        let n = r.varint()?;
        if n > MAX_MOVES as u64 {
            return Err(CodeError::Malformed);
        }
        let mut moves = Vec::with_capacity(n as usize);
        let mut ms = 0u32;
        for _ in 0..n {
            let what = r.varint()?;
            let delta = r.u32()?;
            ms = ms.checked_add(delta).ok_or(CodeError::Malformed)?;
            let cell = usize::try_from(what >> 1).map_err(|_| CodeError::Malformed)?;
            moves.push(Move { kind: if what & 1 == 1 { Kind::Flag } else { Kind::Open }, cell, ms });
        }
        if r.at != body.len() {
            return Err(CodeError::Malformed);
        }
        Ok(Run { level, version, seed, fingerprint, moves })
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], CodeError> {
        let s = self.b.get(self.at..self.at + n).ok_or(CodeError::Malformed)?;
        self.at += n;
        Ok(s)
    }

    fn byte(&mut self) -> Result<u8, CodeError> {
        Ok(self.take(1)?[0])
    }

    fn varint(&mut self) -> Result<u64, CodeError> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = self.byte()?;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err(CodeError::Malformed)
    }

    fn u32(&mut self) -> Result<u32, CodeError> {
        u32::try_from(self.varint()?).map_err(|_| CodeError::Malformed)
    }
}

fn put_varint(b: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        b.push((v as u8) | 0x80);
        v >>= 7;
    }
    b.push(v as u8);
}

fn fnv(b: &[u8]) -> u32 {
    b.iter().fold(0x811c_9dc5u32, |h, &x| (h ^ x as u32).wrapping_mul(0x0100_0193))
}

const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url(b: &[u8]) -> String {
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for chunk in b.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (k, &x)| n | (x as u32) << (16 - 8 * k));
        for k in 0..=chunk.len() {
            s.push(ALPHABET[(n >> (18 - 6 * k) & 63) as usize] as char);
        }
    }
    s
}

fn unbase64url(s: &str) -> Option<Vec<u8>> {
    if s.len() % 4 == 1 {
        return None;
    }
    let digits: Vec<u32> = s.bytes().map(|c| ALPHABET.iter().position(|&a| a == c).map(|p| p as u32)).collect::<Option<_>>()?;
    let mut out = Vec::with_capacity(digits.len() * 3 / 4);
    for chunk in digits.chunks(4) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (k, &d)| n | d << (18 - 6 * k));
        for k in 0..chunk.len() - 1 {
            out.push((n >> (16 - 8 * k)) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Run {
        let moves = vec![
            (Kind::Open, 40, 3.25),
            (Kind::Flag, 0, 3.9),
            (Kind::Flag, 0, 3.9), // the same frame: one millisecond on
            (Kind::Open, 479, 1000.0),
        ];
        Run::from_times(2, [0, 3, 0], 0xdead_beef_0123_4567, 0x1234_5678, &moves)
    }

    #[test]
    fn a_run_code_round_trips() {
        let run = sample();
        assert_eq!(run.moves.iter().map(|m| m.ms).collect::<Vec<_>>(), [0, 650, 651, 996_750]);
        assert_eq!(run.time_ms(), 996_750);
        let code = run.encode();
        assert!(code.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'), "{code}");
        assert_eq!(Run::decode(&code), Ok(run));
    }

    #[test]
    fn base64url_round_trips_every_tail_length() {
        for n in 0..20u8 {
            let b: Vec<u8> = (0..n).map(|k| k.wrapping_mul(97).wrapping_add(n)).collect();
            assert_eq!(unbase64url(&base64url(&b)), Some(b));
        }
    }

    #[test]
    fn a_damaged_code_is_refused() {
        let code = sample().encode();
        assert_eq!(Run::decode(&code[..code.len() - 3]), Err(CodeError::Checksum));
        assert_eq!(Run::decode(&code[..code.len() - 1]), Err(CodeError::Malformed));
        assert_eq!(Run::decode(&format!("{code}!")), Err(CodeError::Malformed));
        assert_eq!(Run::decode(""), Err(CodeError::Malformed));
        let mut flipped = code.clone().into_bytes();
        flipped[12] = if flipped[12] == b'A' { b'B' } else { b'A' };
        assert_eq!(Run::decode(std::str::from_utf8(&flipped).unwrap()), Err(CodeError::Checksum));
        // a later format is refused for what it is, not as garbage
        let mut b = unbase64url(&code).unwrap();
        b.truncate(b.len() - 4);
        b[0] = FORMAT + 1;
        let sum = fnv(&b);
        b.extend_from_slice(&sum.to_le_bytes());
        assert_eq!(Run::decode(&base64url(&b)), Err(CodeError::Format));
        assert_eq!(Run::decode(&"A".repeat(MAX_CODE + 4)), Err(CodeError::Malformed));
    }

    #[test]
    fn versions_parse() {
        assert_eq!(parse_version("0.3.0"), [0, 3, 0]);
        assert_eq!(parse_version("1.12.7-beta"), [1, 12, 0]);
    }
}
