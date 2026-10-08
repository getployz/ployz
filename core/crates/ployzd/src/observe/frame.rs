//! Decoder for the files Docker's `local` log driver writes.
//!
//! Each file is a run of frames, `[u32 BE size][LogEntry protobuf][u32 BE size]`.
//! Docker splits a line longer than 16 KiB into chunks marked partial; the
//! last chunk carries `last`, and a line still open when the container exits
//! never gets one. Docker does not document this format, so a frame that does
//! not parse is skipped and reported, never trusted and never a stop.

use std::io::{self, Write};

/// The largest frame Docker writes.
const MAX_FRAME: usize = 1_000_000;
/// Timestamps outside 2017..2096 mark a frame found by chance during resync.
const PLAUSIBLE_NANOS: std::ops::Range<i64> = 1_500_000_000_000_000_000..4_000_000_000_000_000_000;
/// A reassembled line longer than this is cut, so one endless line cannot
/// exhaust the reader's memory.
const MAX_LINE: usize = 8 << 20;
const HEADER: usize = 4;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stream {
    Stdout,
    Stderr,
}

/// Where a frame sits in a line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Piece {
    Whole,
    /// A chunk of a longer line, with more to come.
    Continues,
    /// The final chunk of a longer line.
    Last,
}

/// One decoded frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Entry<'a> {
    pub ts: i64,
    pub stream: Stream,
    pub chunk: &'a [u8],
    pub piece: Piece,
}

/// What a walk over a file yields.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event<'a> {
    Entry(Entry<'a>),
    /// Bytes from `offset` that held no valid frame.
    Corrupt {
        offset: usize,
        skipped: usize,
    },
}

/// Walks the frames of one file's bytes.
///
/// An incomplete frame at the end is the file Docker is still writing, not
/// corruption; [`Frames::unread_tail`] says how many bytes it held.
pub struct Frames<'a> {
    buf: &'a [u8],
    pos: usize,
    tail: usize,
}

impl<'a> Frames<'a> {
    #[must_use]
    pub fn new(buf: &'a [u8]) -> Self {
        Self {
            buf,
            pos: 0,
            tail: 0,
        }
    }

    /// Bytes at the end of the file that started a frame Docker had not
    /// finished writing when the file was read.
    #[must_use]
    pub fn unread_tail(&self) -> usize {
        self.tail
    }

    fn frame_at(&self, at: usize) -> Option<(usize, Entry<'a>)> {
        let size = read_size(self.buf, at)?;
        if size > MAX_FRAME {
            return None;
        }
        let body_end = at.checked_add(HEADER)?.checked_add(size)?;
        if read_size(self.buf, body_end)? != size {
            return None;
        }
        let entry = decode_entry(self.buf.get(at + HEADER..body_end)?)?;
        PLAUSIBLE_NANOS
            .contains(&entry.ts)
            .then_some((body_end + HEADER, entry))
    }

    fn incomplete_at(&self, at: usize) -> bool {
        read_size(self.buf, at)
            .is_none_or(|size| size <= MAX_FRAME && at + HEADER + size + HEADER > self.buf.len())
    }
}

impl<'a> Iterator for Frames<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        if self.pos >= self.buf.len() {
            return None;
        }
        if let Some((next, entry)) = self.frame_at(self.pos) {
            self.pos = next;
            return Some(Event::Entry(entry));
        }
        if self.incomplete_at(self.pos) {
            self.tail = self.buf.len() - self.pos;
            self.pos = self.buf.len();
            return None;
        }
        let offset = self.pos;
        let resumed = (offset + 1..self.buf.len())
            .find(|&at| self.frame_at(at).is_some())
            .unwrap_or(self.buf.len());
        self.pos = resumed;
        Some(Event::Corrupt {
            offset,
            skipped: resumed - offset,
        })
    }
}

fn read_size(buf: &[u8], at: usize) -> Option<usize> {
    let bytes: [u8; HEADER] = buf.get(at..at.checked_add(HEADER)?)?.try_into().ok()?;
    usize::try_from(u32::from_be_bytes(bytes)).ok()
}

fn varint(buf: &[u8], pos: &mut usize) -> Option<u64> {
    let mut value = 0_u64;
    for shift in 0..10 {
        let byte = *buf.get(*pos)?;
        *pos += 1;
        value |= u64::from(byte & 0x7f) << (7 * shift);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

enum Value<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    Fixed,
}

fn field<'a>(buf: &'a [u8], pos: &mut usize) -> Option<(u64, Value<'a>)> {
    let key = varint(buf, pos)?;
    let number = key >> 3;
    if number == 0 {
        return None;
    }
    let value = match key & 7 {
        0 => Value::Int(varint(buf, pos)?),
        1 | 5 => {
            let width = if key & 7 == 1 { 8 } else { 4 };
            *pos = pos.checked_add(width).filter(|end| *end <= buf.len())?;
            Value::Fixed
        }
        2 => {
            let len = usize::try_from(varint(buf, pos)?).ok()?;
            let end = pos.checked_add(len)?;
            let bytes = buf.get(*pos..end)?;
            *pos = end;
            Value::Bytes(bytes)
        }
        _ => return None,
    };
    Some((number, value))
}

/// Decodes moby's `LogEntry`: 1 source, 2 `time_nano`, 3 line, 4 partial,
/// 5 `PartialLogEntryMetadata` (1 last, 2 id, 3 ordinal).
fn decode_entry(buf: &[u8]) -> Option<Entry<'_>> {
    let mut stream = None;
    let mut ts = 0_i64;
    let mut chunk: &[u8] = &[];
    let mut partial = false;
    let mut last = false;
    let mut pos = 0;
    while pos < buf.len() {
        match field(buf, &mut pos)? {
            (1, Value::Bytes(b"stdout")) => stream = Some(Stream::Stdout),
            (1, Value::Bytes(b"stderr")) => stream = Some(Stream::Stderr),
            (1, _) => return None,
            (2, Value::Int(nanos)) => ts = i64::try_from(nanos).ok()?,
            (3, Value::Bytes(bytes)) => chunk = bytes,
            (4, Value::Int(flag)) => partial = flag != 0,
            (5, Value::Bytes(meta)) => last = partial_is_last(meta)?,
            _ => {}
        }
    }
    let piece = match (partial, last) {
        (false, _) => Piece::Whole,
        (true, false) => Piece::Continues,
        (true, true) => Piece::Last,
    };
    Some(Entry {
        ts,
        stream: stream?,
        chunk,
        piece,
    })
}

fn partial_is_last(meta: &[u8]) -> Option<bool> {
    let mut last = false;
    let mut pos = 0;
    while pos < meta.len() {
        if let (1, Value::Int(flag)) = field(meta, &mut pos)? {
            last = flag != 0;
        }
    }
    Some(last)
}

/// One line of a container's output after its chunks are joined.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Line {
    /// The timestamp of the line's first chunk.
    pub ts: i64,
    pub stream: Stream,
    pub text: Vec<u8>,
    /// False for a line the container never ended, or one cut at `MAX_LINE`.
    pub terminated: bool,
}

/// Joins chunked lines, one open line per stream, across interleaved
/// streams and across a container's files fed in order.
#[derive(Default)]
pub struct Reassembler {
    stdout: Option<Line>,
    stderr: Option<Line>,
}

impl Reassembler {
    /// Feeds one frame and calls `emit` for each line it completes.
    pub fn push(&mut self, entry: &Entry<'_>, mut emit: impl FnMut(Line)) {
        let open = match entry.stream {
            Stream::Stdout => &mut self.stdout,
            Stream::Stderr => &mut self.stderr,
        };
        if entry.piece == Piece::Whole {
            if let Some(unended) = open.take() {
                emit(unended);
            }
            emit(Line {
                ts: entry.ts,
                stream: entry.stream,
                text: entry.chunk.to_vec(),
                terminated: true,
            });
            return;
        }
        let line = open.get_or_insert_with(|| Line {
            ts: entry.ts,
            stream: entry.stream,
            text: Vec::new(),
            terminated: false,
        });
        line.text.extend_from_slice(entry.chunk);
        if entry.piece == Piece::Last {
            line.terminated = true;
        }
        if (line.terminated || line.text.len() >= MAX_LINE)
            && let Some(line) = open.take()
        {
            emit(line);
        }
    }

    /// Lines still open when the container's output ended, oldest first.
    pub fn finish(self) -> impl Iterator<Item = Line> {
        let mut open: Vec<Line> = self.stdout.into_iter().chain(self.stderr).collect();
        open.sort_by_key(|line| line.ts);
        open.into_iter()
    }
}

/// Writes a frame as `docker logs --timestamps` prints it: each chunk under
/// its own timestamp, with a newline only where the line ended.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write_docker_style(entry: &Entry<'_>, out: &mut impl Write) -> io::Result<()> {
    write_rfc3339_nanos(entry.ts, out)?;
    out.write_all(b" ")?;
    out.write_all(entry.chunk)?;
    if entry.piece != Piece::Continues {
        out.write_all(b"\n")?;
    }
    Ok(())
}

/// Writes Unix nanoseconds as Go's `RFC3339NanoFixed`, which Docker uses.
///
/// # Errors
///
/// Returns the writer's error.
pub fn write_rfc3339_nanos(nanos: i64, out: &mut impl Write) -> io::Result<()> {
    let secs = nanos.div_euclid(1_000_000_000);
    let frac = nanos.rem_euclid(1_000_000_000);
    let (year, month, day) = civil_from_days(secs.div_euclid(86_400));
    let of_day = secs.rem_euclid(86_400);
    write!(
        out,
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{frac:09}Z",
        of_day / 3600,
        of_day % 3600 / 60,
        of_day % 60
    )
}

/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The first frame's timestamp in `buf`, skipping corruption.
#[must_use]
pub fn first_ts(buf: &[u8]) -> Option<i64> {
    Frames::new(buf).find_map(|event| match event {
        Event::Entry(entry) => Some(entry.ts),
        Event::Corrupt { .. } => None,
    })
}

/// The last complete frame's timestamp in `buf`, read from its footer when
/// the file ends on a whole frame and by a full walk otherwise.
#[must_use]
pub fn last_ts(buf: &[u8]) -> Option<i64> {
    let from_footer = buf.len().checked_sub(HEADER).and_then(|footer| {
        let size = read_size(buf, footer)?;
        let start = footer.checked_sub(size)?.checked_sub(HEADER)?;
        Frames {
            buf,
            pos: start,
            tail: 0,
        }
        .frame_at(start)
        .map(|(_, entry)| entry.ts)
    });
    from_footer.or_else(|| {
        Frames::new(buf)
            .filter_map(|event| match event {
                Event::Entry(entry) => Some(entry.ts),
                Event::Corrupt { .. } => None,
            })
            .last()
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{Entry, Event, Frames, Line, Piece, Reassembler, Stream, first_ts, last_ts};

    const T0: i64 = 1_760_000_000_000_000_000;

    fn varint(mut value: u64, out: &mut Vec<u8>) {
        while value >= 0x80 {
            out.push(u8::try_from(value & 0x7f).unwrap() | 0x80);
            value >>= 7;
        }
        out.push(u8::try_from(value).unwrap());
    }

    fn bytes_field(number: u64, bytes: &[u8], out: &mut Vec<u8>) {
        varint(number << 3 | 2, out);
        varint(bytes.len() as u64, out);
        out.extend_from_slice(bytes);
    }

    /// Encodes one frame the way moby's `local` driver does.
    pub(crate) fn frame(ts: i64, stream: Stream, chunk: &[u8], piece: Piece) -> Vec<u8> {
        let mut body = Vec::new();
        let source: &[u8] = match stream {
            Stream::Stdout => b"stdout",
            Stream::Stderr => b"stderr",
        };
        bytes_field(1, source, &mut body);
        varint(2 << 3, &mut body);
        varint(u64::try_from(ts).unwrap(), &mut body);
        bytes_field(3, chunk, &mut body);
        if piece != Piece::Whole {
            varint(4 << 3, &mut body);
            varint(1, &mut body);
            let mut meta = Vec::new();
            varint(1 << 3, &mut meta);
            varint(u64::from(piece == Piece::Last), &mut meta);
            bytes_field(2, b"8f1c2a", &mut meta);
            bytes_field(5, &meta, &mut body);
        }
        let size = u32::try_from(body.len()).unwrap().to_be_bytes();
        [&size[..], &body, &size].concat()
    }

    fn entries(buf: &[u8]) -> Vec<Event<'_>> {
        Frames::new(buf).collect()
    }

    fn lines(files: &[&[u8]]) -> Vec<Line> {
        let mut reassembler = Reassembler::default();
        let mut out = Vec::new();
        for file in files {
            for event in Frames::new(file) {
                if let Event::Entry(entry) = event {
                    reassembler.push(&entry, |line| out.push(line));
                }
            }
        }
        out.extend(reassembler.finish());
        out
    }

    fn line(ts: i64, stream: Stream, text: &[u8], terminated: bool) -> Line {
        Line {
            ts,
            stream,
            text: text.to_vec(),
            terminated,
        }
    }

    #[test]
    fn decodes_whole_frames_in_order() {
        let file = [
            frame(T0, Stream::Stdout, b"one", Piece::Whole),
            frame(T0 + 1, Stream::Stderr, b"two", Piece::Whole),
        ]
        .concat();
        assert_eq!(
            entries(&file),
            [
                Event::Entry(Entry {
                    ts: T0,
                    stream: Stream::Stdout,
                    chunk: b"one",
                    piece: Piece::Whole
                }),
                Event::Entry(Entry {
                    ts: T0 + 1,
                    stream: Stream::Stderr,
                    chunk: b"two",
                    piece: Piece::Whole
                }),
            ]
        );
    }

    #[test]
    fn an_unfinished_last_frame_is_a_tail_not_corruption() {
        let whole = frame(T0, Stream::Stdout, b"done", Piece::Whole);
        let next = frame(T0 + 1, Stream::Stdout, b"being written", Piece::Whole);
        for cut in 1..next.len() {
            let file = [&whole[..], next.get(..cut).unwrap()].concat();
            let mut frames = Frames::new(&file);
            assert_eq!(frames.by_ref().count(), 1, "cut at {cut}");
            assert_eq!(frames.unread_tail(), cut);
        }
    }

    #[test]
    fn a_torn_frame_at_a_page_boundary_loses_only_itself() {
        let mut file = Vec::new();
        let mut n = 0_i64;
        while file.len() < 4096 - 40 {
            file.extend(frame(
                T0 + n,
                Stream::Stdout,
                format!("L {n}").as_bytes(),
                Piece::Whole,
            ));
            n += 1;
        }
        let before = n;
        let torn = frame(T0 + n, Stream::Stdout, &[b'x'; 200], Piece::Whole);
        let torn_at = file.len();
        file.extend_from_slice(torn.get(..4096 - torn_at).unwrap());
        n += 1;
        for _ in 0..50 {
            file.extend(frame(
                T0 + n,
                Stream::Stdout,
                format!("L {n}").as_bytes(),
                Piece::Whole,
            ));
            n += 1;
        }

        let events = entries(&file);
        let corrupt: Vec<_> = events
            .iter()
            .filter(|event| matches!(event, Event::Corrupt { .. }))
            .collect();
        assert_eq!(
            corrupt,
            [&Event::Corrupt {
                offset: torn_at,
                skipped: 4096 - torn_at
            }]
        );
        let decoded: Vec<String> = events
            .iter()
            .filter_map(|event| match event {
                Event::Entry(entry) => Some(String::from_utf8(entry.chunk.to_vec()).unwrap()),
                Event::Corrupt { .. } => None,
            })
            .collect();
        let expected: Vec<String> = (0..before)
            .chain(before + 1..n)
            .map(|i| format!("L {i}"))
            .collect();
        assert_eq!(decoded, expected);
    }

    #[test]
    fn garbage_without_a_frame_after_it_is_skipped_to_the_end() {
        let mut file = frame(T0, Stream::Stdout, b"kept", Piece::Whole);
        let garbage_at = file.len();
        file.extend_from_slice(&[0xff; 64]);
        assert!(matches!(
            entries(&file)[..],
            [Event::Entry(_), Event::Corrupt { offset, skipped: 64 }] if offset == garbage_at
        ));
    }

    #[test]
    fn a_frame_with_an_implausible_source_or_time_is_corrupt() {
        let mut bad_source = frame(T0, Stream::Stdout, b"x", Piece::Whole);
        let at = bad_source.iter().position(|&b| b == b'o').unwrap();
        *bad_source.get_mut(at).unwrap() = b'O';
        let early = frame(1_000, Stream::Stdout, b"x", Piece::Whole);
        for file in [bad_source, early] {
            assert!(matches!(
                entries(&file)[..],
                [Event::Corrupt { offset: 0, .. }]
            ));
        }
    }

    #[test]
    fn chunks_join_per_stream_across_interleaving_and_files() {
        let first = [
            frame(T0, Stream::Stdout, b"aa", Piece::Continues),
            frame(T0 + 1, Stream::Stderr, b"err", Piece::Whole),
            frame(T0 + 2, Stream::Stderr, b"xx", Piece::Continues),
        ]
        .concat();
        let second = [
            frame(T0 + 3, Stream::Stdout, b"bb", Piece::Continues),
            frame(T0 + 4, Stream::Stderr, b"yy", Piece::Last),
            frame(T0 + 5, Stream::Stdout, b"", Piece::Last),
            frame(T0 + 6, Stream::Stdout, b"open", Piece::Continues),
        ]
        .concat();
        assert_eq!(
            lines(&[&first, &second]),
            [
                line(T0 + 1, Stream::Stderr, b"err", true),
                line(T0 + 2, Stream::Stderr, b"xxyy", true),
                line(T0, Stream::Stdout, b"aabb", true),
                line(T0 + 6, Stream::Stdout, b"open", false),
            ]
        );
    }

    #[test]
    fn a_whole_line_closes_a_chunked_line_that_never_ended() {
        let file = [
            frame(T0, Stream::Stdout, b"cut off", Piece::Continues),
            frame(T0 + 1, Stream::Stdout, b"after restart", Piece::Whole),
        ]
        .concat();
        assert_eq!(
            lines(&[&file]),
            [
                line(T0, Stream::Stdout, b"cut off", false),
                line(T0 + 1, Stream::Stdout, b"after restart", true),
            ]
        );
    }

    #[test]
    fn first_and_last_timestamps_skip_damage_and_tails() {
        let mut file = vec![0xee; 10];
        file.extend(frame(T0, Stream::Stdout, b"a", Piece::Whole));
        file.extend(frame(T0 + 9, Stream::Stdout, b"b", Piece::Whole));
        assert_eq!(first_ts(&file), Some(T0));
        assert_eq!(last_ts(&file), Some(T0 + 9));
        let partial = frame(T0 + 20, Stream::Stdout, b"c", Piece::Whole);
        file.extend_from_slice(partial.get(..5).unwrap());
        assert_eq!(last_ts(&file), Some(T0 + 9));
        assert_eq!(first_ts(&[]), None);
        assert_eq!(last_ts(&[]), None);
    }

    #[test]
    fn timestamps_print_as_docker_prints_them() {
        let mut out = Vec::new();
        super::write_rfc3339_nanos(1_700_000_000_000_000_042, &mut out).unwrap();
        assert_eq!(out, b"2023-11-14T22:13:20.000000042Z");
        out.clear();
        super::write_rfc3339_nanos(951_782_400_000_000_000, &mut out).unwrap();
        assert_eq!(out, b"2000-02-29T00:00:00.000000000Z");
    }
}
