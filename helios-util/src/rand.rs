use std::iter::repeat_with;

pub use fastrand::Rng as PseudoRng;
pub use getrandom::{self as os_random, Error as OsError};

pub trait RngExt {
    /// Returns a string of the given length that is the result of pseudo-randomly
    /// choosing characters from the given character set.
    ///
    /// ```ignore
    /// use helios_store::util::rand::{self, PseudoRng, RngExt};
    ///
    /// let mut rng = PseudoRng::new();
    /// let s = rng.string(rand::ALPHA_NUM, 32);
    /// assert_eq!(s.len(), 32);
    /// println!("{s}"); // eg.: "nY8lUBtEIZyX0jmVpFAouw72gTNQR9s3"
    ///
    /// let s = rng.string(rand::ALPHA_NUM_LC, 10);
    /// assert_eq!(s.len(), 10);
    /// assert_eq!(s.to_lowercase(), s);
    /// println!("{s}"); // eg.: "y1qkt546cn"
    ///
    /// let s = rng.string(rand::ALPHA_NUM_UC, 10);
    /// assert_eq!(s.len(), 10);
    /// assert_eq!(s.to_uppercase(), s);
    /// println!("{s}"); // eg.: "5LHGEVU6KQ"
    /// ```
    fn string(&mut self, charset: impl AsRef<[u8]>, len: usize) -> String;

    /// Returns an error if the RNG fails to get a suitable random seed.
    fn reseed(&mut self) -> Result<(), OsError>;
}

impl RngExt for PseudoRng {
    fn string(&mut self, charset: impl AsRef<[u8]>, len: usize) -> String {
        let charset = charset.as_ref();
        let max = charset.len();
        repeat_with(|| charset[self.usize(..max)] as char)
            .take(len)
            .collect()
    }

    fn reseed(&mut self) -> Result<(), OsError> {
        let seed = os_random::u64()?;
        self.seed(seed);
        Ok(())
    }
}

/// All lowercase letters and digits.
pub const ALPHA_NUM_LC: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
