#![no_main]

use libfuzzer_sys::fuzz_target;

// A caption body becomes cues, and the cues become a track. The second half is
// included because a body can state times that the writer must handle: a cue
// that ends before it starts is refused in `cues`, and anything that survives
// has to produce samples the writer accepts.
fuzz_target!(|data: &[u8]| {
    if let Ok(cues) = tafas::subtitles::cues(data) {
        let _ = tafas::subtitles::track(&cues);
    }
});
