//! The code a host shows and a guest types.

use std::fmt;

/// Characters a code is drawn from: the capitals and digits of the app's
/// password alphabet (`src/lib/password.ts`), without the look-alikes `I`,
/// `O`, `0` and `1`. 32 of them, so a random byte maps onto one without bias.
pub const ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Characters in a code: 8 × 5 bits, 2⁴⁰ codes.
pub const LEN: usize = 8;

/// A code. Said aloud and typed, so it is read case-insensitively, with the
/// spaces and dashes people put in. `Debug` hides it.
#[derive(Clone, PartialEq, Eq)]
pub struct Code([u8; LEN]);

impl Code {
    /// A fresh code from the system's CSPRNG.
    pub fn generate() -> Self {
        Self::from_random(super::random())
    }

    /// The code `bytes` draw: each one picks a character.
    pub fn from_random(bytes: [u8; LEN]) -> Self {
        Self(bytes.map(|b| ALPHABET[usize::from(b) % ALPHABET.len()]))
    }

    /// What someone typed, if it is a code: [`LEN`] characters of the
    /// alphabet once lowercase is raised and spaces and dashes are dropped.
    pub fn parse(typed: &str) -> Option<Self> {
        let mut out = [0u8; LEN];
        let mut n = 0;
        for c in typed.chars() {
            if c.is_whitespace() || c == '-' {
                continue;
            }
            let c = c.to_ascii_uppercase();
            if !c.is_ascii() || !ALPHABET.contains(&(c as u8)) || n == LEN {
                return None;
            }
            out[n] = c as u8;
            n += 1;
        }
        (n == LEN).then_some(Self(out))
    }

    /// The code as it is shown: two groups of four, `ABCD-EFGH`.
    pub fn grouped(&self) -> String {
        let text = self.as_str();
        format!("{}-{}", &text[..4], &text[4..])
    }

    /// The code itself, for the exchange.
    pub fn as_str(&self) -> &str {
        // Every byte is from the ASCII alphabet.
        std::str::from_utf8(&self.0).unwrap_or_default()
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Code(***)")
    }
}
