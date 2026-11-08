#![no_main]

use libfuzzer_sys::fuzz_target;

// Brace matching in a page: a `}` inside a string, an escaped quote and an
// unterminated object are the three inputs that break a naive scanner.
fuzz_target!(|data: &[u8]| {
    let Ok(page) = std::str::from_utf8(data) else {
        return;
    };
    let _ = tafas::sites::json_after(page, "ytInitialPlayerResponse");
    let _ = tafas::sites::json_after(page, "__UNIVERSAL_DATA_FOR_REHYDRATION__");
});
