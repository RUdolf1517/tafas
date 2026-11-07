//! A second MP4 reader, for the tests only.
//!
//! [docs/guarantees.md §6](../../docs/guarantees.md) promises that a written
//! file is checked by a parser that shares no code with the writer, because a
//! verifier living beside the writer can be wrong in the same way twice. This
//! module walks boxes from the ISO-BMFF definition — a size, a type, a version —
//! and knows nothing about `tafas::mux` or `tafas::mp4write`; it does not even
//! import them.
//!
//! It is deliberately small: enough to say which tracks a file has, what codec
//! each one claims, how many samples there are and where they point. That is
//! exactly the set of claims a reader makes before it plays anything.

/// One sample as the tables describe it.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// Absolute offset of the payload in the file.
    pub offset: usize,
    pub size: usize,
    pub keyframe: bool,
}

#[derive(Debug, Clone)]
pub struct Track {
    /// `vide`, `soun`, … from `hdlr`.
    pub handler: String,
    /// The four-character code of the first `stsd` entry: `avc1`, `mp4a`.
    pub codec: String,
    pub timescale: u32,
    pub samples: Vec<Sample>,
}

impl Track {
    pub fn keyframes(&self) -> usize {
        self.samples.iter().filter(|sample| sample.keyframe).count()
    }
}

#[derive(Debug, Clone)]
pub struct Movie {
    pub timescale: u32,
    pub duration: u64,
    pub tracks: Vec<Track>,
    /// Payload ranges of every `mdat`, as `(start, end)`.
    pub mdats: Vec<(usize, usize)>,
}

impl Movie {
    pub fn track(&self, handler: &str) -> Option<&Track> {
        self.tracks.iter().find(|track| track.handler == handler)
    }

    pub fn sample_total(&self) -> usize {
        self.tracks.iter().map(|track| track.samples.len()).sum()
    }

    /// Whether a sample's bytes lie inside an `mdat`. A chunk offset pointing at
    /// the file header is the mistake this exists to catch.
    pub fn inside_mdat(&self, sample: &Sample) -> bool {
        let end = sample.offset.saturating_add(sample.size);
        self.mdats
            .iter()
            .any(|(start, stop)| sample.offset >= *start && end <= *stop)
    }
}

// --- the walker ------------------------------------------------------------

struct Node<'a> {
    kind: [u8; 4],
    /// Payload, i.e. everything after the 8-byte (or 16-byte) header.
    body: &'a [u8],
    /// Absolute offset of the payload in the file.
    body_at: usize,
}

fn nodes(data: &[u8], base: usize) -> Result<Vec<Node<'_>>, String> {
    let mut out = Vec::new();
    let mut rest = data;
    let mut at = base;
    while !rest.is_empty() {
        if rest.len() < 8 {
            return Err(format!(
                "{at}: {} bytes are fewer than a box header",
                rest.len()
            ));
        }
        let declared = u32::from_be_bytes(rest[0..4].try_into().expect("four bytes")) as usize;
        let kind: [u8; 4] = rest[4..8].try_into().expect("four bytes");
        let (size, header) = match declared {
            0 => (rest.len(), 8),
            1 => {
                if rest.len() < 16 {
                    return Err(format!("{}: a 64-bit size is truncated", name(&kind)));
                }
                let size = u64::from_be_bytes(rest[8..16].try_into().expect("eight bytes"));
                (
                    usize::try_from(size).map_err(|_| "a size past this machine".to_string())?,
                    16,
                )
            }
            size => (size, 8),
        };
        if size < header || size > rest.len() {
            return Err(format!(
                "{}: a box declares {size} bytes with {} left",
                name(&kind),
                rest.len()
            ));
        }
        out.push(Node {
            kind,
            body: &rest[header..size],
            body_at: at + header,
        });
        rest = &rest[size..];
        at += size;
    }
    Ok(out)
}

fn name(kind: &[u8; 4]) -> String {
    String::from_utf8_lossy(kind).into_owned()
}

fn child<'a>(node: &Node<'a>, kind: &[u8; 4]) -> Result<Option<Node<'a>>, String> {
    Ok(nodes(node.body, node.body_at)?
        .into_iter()
        .find(|child| &child.kind == kind))
}

fn child_or_fail<'a>(node: &Node<'a>, kind: &[u8; 4]) -> Result<Node<'a>, String> {
    child(node, kind)?.ok_or_else(|| format!("{} has no {}", name(&node.kind), name(kind)))
}

fn require(body: &[u8], count: usize, what: &str) -> Result<(), String> {
    if body.len() < count {
        return Err(format!("{what}: {} bytes, needed {count}", body.len()));
    }
    Ok(())
}

fn u32_at(body: &[u8], at: usize, what: &str) -> Result<u32, String> {
    require(body, at + 4, what)?;
    Ok(u32::from_be_bytes(
        body[at..at + 4].try_into().expect("four bytes"),
    ))
}

fn u64_at(body: &[u8], at: usize, what: &str) -> Result<u64, String> {
    require(body, at + 8, what)?;
    Ok(u64::from_be_bytes(
        body[at..at + 8].try_into().expect("eight bytes"),
    ))
}

/// Header of a full box: `(version, payload after the four header bytes)`.
fn full(body: &[u8], what: &str) -> Result<(u8, &[u8]), String> {
    require(body, 4, what)?;
    Ok((body[0], &body[4..]))
}

pub fn read(data: &[u8]) -> Result<Movie, String> {
    let top = nodes(data, 0)?;
    let mdats = top
        .iter()
        .filter(|node| &node.kind == b"mdat")
        .map(|node| (node.body_at, node.body_at + node.body.len()))
        .collect::<Vec<_>>();

    let moov = top
        .iter()
        .find(|node| &node.kind == b"moov")
        .ok_or_else(|| "no moov box: the file has no index".to_string())?;

    let mvhd = child_or_fail(moov, b"mvhd")?;
    let (version, payload) = full(mvhd.body, "mvhd")?;
    let (timescale, duration) = match version {
        0 => (
            u32_at(payload, 8, "mvhd timescale")?,
            u32_at(payload, 12, "mvhd duration")? as u64,
        ),
        _ => (
            u32_at(payload, 16, "mvhd timescale")?,
            u64_at(payload, 20, "mvhd duration")?,
        ),
    };

    let mut tracks = Vec::new();
    for node in nodes(moov.body, moov.body_at)? {
        if &node.kind != b"trak" {
            continue;
        }
        tracks.push(read_track(&node)?);
    }

    Ok(Movie {
        timescale,
        duration,
        tracks,
        mdats,
    })
}

fn read_track(trak: &Node<'_>) -> Result<Track, String> {
    let mdia = child_or_fail(trak, b"mdia")?;

    let mdhd = child_or_fail(&mdia, b"mdhd")?;
    let (version, payload) = full(mdhd.body, "mdhd")?;
    let timescale = match version {
        0 => u32_at(payload, 8, "mdhd timescale")?,
        _ => u32_at(payload, 16, "mdhd timescale")?,
    };

    let hdlr = child_or_fail(&mdia, b"hdlr")?;
    let (_, hdlr_payload) = full(hdlr.body, "hdlr")?;
    require(hdlr_payload, 8, "hdlr")?;
    let handler = String::from_utf8_lossy(&hdlr_payload[4..8]).into_owned();

    let stbl = child_or_fail(&child_or_fail(&mdia, b"minf")?, b"stbl")?;

    let stsd = child_or_fail(&stbl, b"stsd")?;
    let (_, stsd_payload) = full(stsd.body, "stsd")?;
    // entry_count, then the entries themselves, each an ordinary box.
    let entries = nodes(stsd_payload.get(4..).unwrap_or_default(), stsd.body_at + 4)?;
    let codec = entries
        .first()
        .map(|entry| name(&entry.kind))
        .unwrap_or_default();

    let sizes = read_stsz(&child_or_fail(&stbl, b"stsz")?)?;
    let chunk_offsets = match child(&stbl, b"stco")? {
        Some(stco) => read_stco(&stco, false)?,
        None => read_stco(&child_or_fail(&stbl, b"co64")?, true)?,
    };
    let stsc = read_stsc(&child_or_fail(&stbl, b"stsc")?)?;
    let keyframes = match child(&stbl, b"stss")? {
        Some(stss) => read_stss(&stss)?,
        // No `stss` at all means every sample is a sync sample.
        None => (1..=sizes.len()).collect(),
    };

    // Chunk offsets plus samples-per-chunk give the offset of every sample;
    // sample sizes come from `stsz` in the same order.
    let mut offsets = Vec::new();
    for (index, entry) in stsc.iter().enumerate() {
        let first_chunk = entry.0;
        let per_chunk = entry.1;
        let last_chunk = match stsc.get(index + 1) {
            Some(next) => next.0.saturating_sub(1),
            None => chunk_offsets.len(),
        };
        if first_chunk == 0 || last_chunk < first_chunk {
            continue;
        }
        for chunk in first_chunk..=last_chunk {
            let Some(base) = chunk_offsets.get(chunk - 1).copied() else {
                return Err(format!("stsc names chunk {chunk}, stco lists fewer"));
            };
            let mut cursor = base;
            for _ in 0..per_chunk {
                offsets.push(cursor);
                let size = sizes.get(offsets.len() - 1).copied().unwrap_or(0);
                cursor += size;
            }
        }
    }

    let samples = sizes
        .iter()
        .enumerate()
        .map(|(index, size)| Sample {
            offset: offsets.get(index).copied().unwrap_or(0),
            size: *size,
            keyframe: keyframes.contains(&(index + 1)),
        })
        .collect();

    Ok(Track {
        handler,
        codec,
        timescale,
        samples,
    })
}

fn read_stsz(stsz: &Node<'_>) -> Result<Vec<usize>, String> {
    let (_, payload) = full(stsz.body, "stsz")?;
    let uniform = u32_at(payload, 0, "stsz sample_size")? as usize;
    let count = u32_at(payload, 4, "stsz sample_count")? as usize;
    if count > 4_000_000 {
        return Err(format!("stsz claims {count} samples"));
    }
    if uniform > 0 {
        return Ok(vec![uniform; count]);
    }
    let mut sizes = Vec::with_capacity(count);
    for index in 0..count {
        sizes.push(u32_at(payload, 8 + index * 4, "stsz entry")? as usize);
    }
    Ok(sizes)
}

fn read_stco(stco: &Node<'_>, wide: bool) -> Result<Vec<usize>, String> {
    let (_, payload) = full(stco.body, "stco")?;
    let count = u32_at(payload, 0, "stco entry_count")? as usize;
    let width = if wide { 8 } else { 4 };
    let mut offsets = Vec::with_capacity(count);
    for index in 0..count {
        let at = 4 + index * width;
        let value = if wide {
            usize::try_from(u64_at(payload, at, "co64 entry")?)
                .map_err(|_| "a chunk offset past this machine".to_string())?
        } else {
            u32_at(payload, at, "stco entry")? as usize
        };
        offsets.push(value);
    }
    Ok(offsets)
}

/// `(first_chunk, samples_per_chunk)` per entry; the third field is a
/// sample-description index this reader does not need.
fn read_stsc(stsc: &Node<'_>) -> Result<Vec<(usize, usize)>, String> {
    let (_, payload) = full(stsc.body, "stsc")?;
    let count = u32_at(payload, 0, "stsc entry_count")? as usize;
    let mut entries = Vec::with_capacity(count);
    for index in 0..count {
        let at = 4 + index * 12;
        entries.push((
            u32_at(payload, at, "stsc first_chunk")? as usize,
            u32_at(payload, at + 4, "stsc samples_per_chunk")? as usize,
        ));
    }
    if entries.is_empty() {
        // A table with no entries still describes nothing; one entry covering
        // every chunk would be the honest empty case, but an MP4 without it has
        // no samples at all.
        entries.push((1, 0));
    }
    Ok(entries)
}

fn read_stss(stss: &Node<'_>) -> Result<Vec<usize>, String> {
    let (_, payload) = full(stss.body, "stss")?;
    let count = u32_at(payload, 0, "stss entry_count")? as usize;
    let mut numbers = Vec::with_capacity(count);
    for index in 0..count {
        numbers.push(u32_at(payload, 4 + index * 4, "stss entry")? as usize);
    }
    Ok(numbers)
}
