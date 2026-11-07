#![no_main]

use std::path::PathBuf;

use libfuzzer_sys::fuzz_target;

// The remuxer and the verifier take a path, because the verifier re-reads its
// own output from disk. So the target owns a temporary file, and that is why it
// is slower per execution than the three slice-based targets: a corpus here is
// worth a few hundred executions a second, not a hundred thousand.
//
// Two things are asked of the same bytes: `verify` must walk any file at all
// without panicking, and `remux` must refuse anything that is not two ISO-BMFF
// tracks rather than guessing at offsets.
fuzz_target!(|data: &[u8]| {
    let dir = std::env::temp_dir().join(format!("tafas-fuzz-mux-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    let input = dir.join("input.bin");
    let output = dir.join("output.mp4");
    if std::fs::write(&input, data).is_err() {
        return;
    }

    let _ = tafas::mux::verify(&input);
    let _ = tafas::mux::remux(&input, &input, &output);

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&output);
    let _: PathBuf = dir;
});
