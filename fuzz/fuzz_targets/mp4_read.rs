#![no_main]

use libfuzzer_sys::fuzz_target;

// Two readers over the same bytes: the sample tables (`stbl`) and the track
// header (`tkhd`). Both take a slice and return a `Result`/`Option`; a panic is
// the bug this target exists to find, not the input that caused it.
fuzz_target!(|data: &[u8]| {
    let _ = tafas::mp4read::plain(data);
    let _ = tafas::mp4read::dimensions(data);
});
