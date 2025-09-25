use std::fs;
use std::io::{self, Write};
use std::path::Path;

use super::crypto::{ALPHA_NUM, pseudorandom_string};

/// Atomically creates a file with the given contents, overwriting
/// it if one exists.
///
/// This function will first write the buffer into a new file that
/// resides in the same directory as the desired file and then do
/// the complete sync/rename dance to ensure the buffer is safely
/// written to disk. If this function returns successfully, you can
/// be reasonably sure the write completed durably.
///
/// Read: [Ensuring data reaches to disk](https://lwn.net/Articles/457667/).
pub fn safe_write_all<P: AsRef<Path>, B: AsRef<[u8]>>(path: P, buf: B) -> io::Result<()> {
    // create temp file
    let tmp_ext = "sync-".to_owned() + &pseudorandom_string(ALPHA_NUM, 6);
    let tmp_path = path.as_ref().with_extension(tmp_ext);
    let mut tmp_file = fs::File::create(tmp_path.clone())?;

    // write given contents and sync to disk
    tmp_file.write_all(buf.as_ref())?;
    tmp_file.flush()?;
    tmp_file.sync_all()?;
    drop(tmp_file);

    // rename tmp file to destination
    fs::rename(&tmp_path, path.as_ref())
}
