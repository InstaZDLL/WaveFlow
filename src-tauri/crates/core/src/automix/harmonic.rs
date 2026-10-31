//! Which keys mix well, on the Camelot wheel.
//!
//! The wheel puts the 12 major keys round a circle of fifths (`1B` to
//! `12B`, `8B` = C major) and each one's relative minor beside it (`8A` =
//! A minor). Two keys sound well together when they are the same, a step
//! apart on the same ring (a fifth up or down), or the two of one number
//! (relative major and minor): they share all or all but one of their
//! notes.
//!
//! Keys arrive as the tag or the analysis wrote them, and [`parse`] reads
//! every form the frontend's `parseMusicalKey` (`src/lib/musicalKey.ts`)
//! reads: `Am`, `A minor`, `Bbm`, `F#`, `E♭ major`, Camelot `8A`, Open Key
//! `1m`.

/// A key as a tonic pitch class (0 = C) and a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub tonic: u8,
    pub minor: bool,
}

impl Key {
    /// Its place on the Camelot wheel: the number, 1 to 12, and whether
    /// it is on the minor (`A`) ring.
    pub fn camelot(self) -> (u8, bool) {
        // A minor key sits beside its relative major, three semitones up.
        let major_tonic = if self.minor {
            (self.tonic + 3) % 12
        } else {
            self.tonic % 12
        };
        // `8B` is C and each number is a fifth (7 semitones) up, so the
        // number is 8 + 7 × tonic, mod 12 (7 is its own inverse mod 12).
        let n = (8 + 7 * u32::from(major_tonic)) % 12;
        (if n == 0 { 12 } else { n as u8 }, self.minor)
    }
}

/// Whether two keys mix well: the same, a step apart on one ring, or a
/// relative major and minor.
pub fn compatible(a: Key, b: Key) -> bool {
    let (na, ma) = a.camelot();
    let (nb, mb) = b.camelot();
    if na == nb {
        return true;
    }
    let step = (i32::from(na) - i32::from(nb)).rem_euclid(12);
    ma == mb && (step == 1 || step == 11)
}

/// Read a key the way taggers and DJ tools write one, or the analysis
/// stores one. `None` for anything else.
pub fn parse(raw: &str) -> Option<Key> {
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let lower = text.to_lowercase();

    // Camelot `8A` / Open Key `1m`: a number 1 to 12, a letter.
    if let Some(letter) = lower.chars().last() {
        let digits = lower[..lower.len() - letter.len_utf8()].trim();
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            let n: u8 = digits.parse().ok()?;
            if !(1..=12).contains(&n) {
                return None;
            }
            let (camelot, minor) = match letter {
                'a' => (n, true),
                'b' => (n, false),
                // Open Key: 1d is C major, Camelot 8B.
                'm' => ((n + 6) % 12 + 1, true),
                'd' => ((n + 6) % 12 + 1, false),
                _ => return None,
            };
            let major_tonic = (u32::from(camelot) * 7 + 4) % 12;
            let tonic = if minor {
                (major_tonic + 9) % 12
            } else {
                major_tonic
            };
            return Some(Key {
                tonic: tonic as u8,
                minor,
            });
        }
    }

    let mut chars = lower.chars();
    let mut tonic: i32 = match chars.next()? {
        'c' => 0,
        'd' => 2,
        'e' => 4,
        'f' => 5,
        'g' => 7,
        'a' => 9,
        'b' => 11,
        _ => return None,
    };
    let rest: String = chars.collect();
    let rest = rest.trim_start();
    let rest = if let Some(r) = rest.strip_prefix(['#', '♯']) {
        tonic += 1;
        r
    } else if let Some(r) = rest.strip_prefix(['b', '♭']) {
        // `b` is a flat only where a mode can still follow it: `Bb`,
        // `Ebm`, `Eb major` — never the `b` of nothing else.
        tonic -= 1;
        r
    } else {
        rest
    };
    let mode: String = rest
        .trim()
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | '.'))
        .collect();
    let minor = match mode.as_str() {
        "" | "maj" | "major" | "dur" => false,
        "m" | "min" | "minor" | "moll" => true,
        _ => return None,
    };
    Some(Key {
        tonic: tonic.rem_euclid(12) as u8,
        minor,
    })
}

#[cfg(test)]
mod tests;
