//! The ratings index: fixed-size records sorted by numeric title id.
//!
//! File layout, little endian:
//! - header, 16 bytes: magic `RMIR`, format version `u16`, reserved `u16`,
//!   record count `u32`, reserved `u32`
//! - records, 9 bytes each: tconst number `u32`, rating × 10 `u8`, votes `u32`
//!
//! The dataset is sorted as text, which isn't numeric order once 8-digit ids appear
//! (`tt10000000` sorts before `tt1000001`). The builder therefore sorts after parsing, as an
//! external merge sort: sorted chunks of 256 K records spill to a run file and are k-way
//! merged into the final index, so peak memory stays near 3 MB.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const RECORD_SIZE: usize = 9;
const HEADER_SIZE: u64 = 16;
const MAGIC: [u8; 4] = *b"RMIR";
const FORMAT_VERSION: u16 = 1;
const DEFAULT_CHUNK_RECORDS: usize = 256 * 1024;
/// Once the search window is this small, it is read in one go.
const WINDOW_RECORDS: u64 = 64;
/// Longest line accepted; real rows are about 25 bytes.
const MAX_LINE: usize = 4096;

type Record = [u8; RECORD_SIZE];

/// An IMDb rating: the average × 10 (so 7.8 is 78) and the vote count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rating {
    pub rating_x10: u8,
    pub votes: u32,
}

/// `tt0133093` to 133093.
pub fn parse_tconst(id: &str) -> Option<u32> {
    let digits = id.strip_prefix("tt")?;
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// One TSV row, `tt0133093\t8.7\t2200000`. The header and malformed rows give `None`.
pub fn parse_row(line: &[u8]) -> Option<(u32, Rating)> {
    let line = line.strip_suffix(b"\r").unwrap_or(line);
    let mut fields = line.split(|&b| b == b'\t');
    let id = parse_tconst(std::str::from_utf8(fields.next()?).ok()?)?;
    let rating_x10 = parse_rating_x10(fields.next()?)?;
    let votes = parse_u32(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    Some((id, Rating { rating_x10, votes }))
}

fn parse_u32(s: &[u8]) -> Option<u32> {
    if s.is_empty() || s.len() > 10 || !s.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let value = s
        .iter()
        .fold(0u64, |acc, &b| acc * 10 + u64::from(b - b'0'));
    u32::try_from(value).ok()
}

/// "7.8" to 78 without floating point. Accepts "10", "10.0" and "5".
fn parse_rating_x10(s: &[u8]) -> Option<u8> {
    let (whole, fraction) = match s.iter().position(|&b| b == b'.') {
        Some(dot) => (&s[..dot], &s[dot + 1..]),
        None => (s, &s[s.len()..]),
    };
    if !fraction.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let tenths = fraction.first().map_or(0, |&b| u32::from(b - b'0'));
    let value = parse_u32(whole)?.checked_mul(10)?.checked_add(tenths)?;
    u8::try_from(value).ok().filter(|&v| v <= 100)
}

fn encode(id: u32, rating: Rating) -> Record {
    let mut record = [0u8; RECORD_SIZE];
    record[0..4].copy_from_slice(&id.to_le_bytes());
    record[4] = rating.rating_x10;
    record[5..9].copy_from_slice(&rating.votes.to_le_bytes());
    record
}

fn record_id(record: &[u8]) -> u32 {
    u32::from_le_bytes([record[0], record[1], record[2], record[3]])
}

fn record_rating(record: &[u8]) -> Rating {
    Rating {
        rating_x10: record[4],
        votes: u32::from_le_bytes([record[5], record[6], record[7], record[8]]),
    }
}

fn header(count: u32) -> [u8; HEADER_SIZE as usize] {
    let mut h = [0u8; HEADER_SIZE as usize];
    h[0..4].copy_from_slice(&MAGIC);
    h[4..6].copy_from_slice(&FORMAT_VERSION.to_le_bytes());
    h[8..12].copy_from_slice(&count.to_le_bytes());
    h
}

/// Builds an index from decompressed TSV bytes written in chunks of any size.
pub struct IndexBuilder {
    run_path: PathBuf,
    run_file: BufWriter<File>,
    /// (first record, record count) of each sorted run in the run file.
    runs: Vec<(u64, u64)>,
    chunk: Vec<Record>,
    chunk_limit: usize,
    partial: Vec<u8>,
    spilled: u64,
    skipped_rows: u64,
}

impl IndexBuilder {
    pub fn new(work_dir: &Path) -> io::Result<IndexBuilder> {
        IndexBuilder::with_chunk_records(work_dir, DEFAULT_CHUNK_RECORDS)
    }

    /// Smaller chunks force several runs; tests use this to exercise the merge.
    pub fn with_chunk_records(work_dir: &Path, chunk_limit: usize) -> io::Result<IndexBuilder> {
        fs::create_dir_all(work_dir)?;
        let run_path = work_dir.join("ratings.runs.tmp");
        let run_file = BufWriter::with_capacity(64 * 1024, File::create(&run_path)?);
        let chunk_limit = chunk_limit.max(1);
        Ok(IndexBuilder {
            run_path,
            run_file,
            runs: Vec::new(),
            chunk: Vec::with_capacity(chunk_limit.min(DEFAULT_CHUNK_RECORDS)),
            chunk_limit,
            partial: Vec::new(),
            spilled: 0,
            skipped_rows: 0,
        })
    }

    /// Rows that were neither the header nor valid.
    pub fn skipped_rows(&self) -> u64 {
        self.skipped_rows
    }

    fn push_line(&mut self, line: &[u8]) -> io::Result<()> {
        if line.is_empty() {
            return Ok(());
        }
        match parse_row(line) {
            Some((id, rating)) => {
                self.chunk.push(encode(id, rating));
                if self.chunk.len() >= self.chunk_limit {
                    self.spill()?;
                }
            }
            None if line.starts_with(b"tconst") => {}
            None => self.skipped_rows += 1,
        }
        Ok(())
    }

    /// Sorts the current chunk and appends it to the run file as one run.
    fn spill(&mut self) -> io::Result<()> {
        if self.chunk.is_empty() {
            return Ok(());
        }
        self.chunk.sort_unstable_by_key(|r| record_id(r));
        for record in &self.chunk {
            self.run_file.write_all(record)?;
        }
        let count = self.chunk.len() as u64;
        self.runs.push((self.spilled, count));
        self.spilled += count;
        self.chunk.clear();
        Ok(())
    }

    /// Writes the merged, de-duplicated index to `out` and returns its record count.
    /// The run file is deleted and every buffer is freed on return.
    pub fn finish(mut self, out: &Path) -> io::Result<u64> {
        if !self.partial.is_empty() {
            let line = std::mem::take(&mut self.partial);
            self.push_line(&line)?;
        }
        self.spill()?;
        let IndexBuilder {
            run_path,
            run_file,
            runs,
            ..
        } = self;
        run_file.into_inner().map_err(|e| e.into_error())?.sync_all()?;
        let result = merge_runs(&run_path, &runs, out);
        let _ = fs::remove_file(&run_path);
        result
    }
}

impl Write for IndexBuilder {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut rest = buf;
        while let Some(newline) = rest.iter().position(|&b| b == b'\n') {
            let (line, tail) = rest.split_at(newline);
            if self.partial.is_empty() {
                self.push_line(line)?;
            } else {
                let mut joined = std::mem::take(&mut self.partial);
                joined.extend_from_slice(line);
                self.push_line(&joined)?;
                joined.clear();
                self.partial = joined;
            }
            rest = &tail[1..];
        }
        self.partial.extend_from_slice(rest);
        if self.partial.len() > MAX_LINE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ratings data has an overlong line",
            ));
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct RunReader {
    reader: BufReader<io::Take<File>>,
}

impl RunReader {
    fn open(run_path: &Path, first: u64, count: u64) -> io::Result<RunReader> {
        let mut file = File::open(run_path)?;
        file.seek(SeekFrom::Start(first * RECORD_SIZE as u64))?;
        Ok(RunReader {
            reader: BufReader::with_capacity(32 * 1024, file.take(count * RECORD_SIZE as u64)),
        })
    }

    fn next_record(&mut self) -> io::Result<Option<Record>> {
        let mut record = [0u8; RECORD_SIZE];
        match self.reader.read_exact(&mut record) {
            Ok(()) => Ok(Some(record)),
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// K-way merge of the sorted runs. Duplicate ids keep their first record.
fn merge_runs(run_path: &Path, runs: &[(u64, u64)], out: &Path) -> io::Result<u64> {
    let mut writer = BufWriter::with_capacity(64 * 1024, File::create(out)?);
    writer.write_all(&[0u8; HEADER_SIZE as usize])?;

    let mut readers = runs
        .iter()
        .map(|&(first, count)| RunReader::open(run_path, first, count))
        .collect::<io::Result<Vec<_>>>()?;
    let mut heap = BinaryHeap::with_capacity(readers.len());
    for (i, reader) in readers.iter_mut().enumerate() {
        if let Some(record) = reader.next_record()? {
            heap.push(Reverse((record_id(&record), i, record)));
        }
    }

    let mut written: u64 = 0;
    let mut last_id = None;
    while let Some(Reverse((id, run, record))) = heap.pop() {
        if last_id != Some(id) {
            writer.write_all(&record)?;
            written += 1;
            last_id = Some(id);
        }
        if let Some(next) = readers[run].next_record()? {
            heap.push(Reverse((record_id(&next), run, next)));
        }
    }

    let count = u32::try_from(written)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "too many ratings"))?;
    let mut file = writer.into_inner().map_err(|e| e.into_error())?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&header(count))?;
    file.sync_all()?;
    Ok(written)
}

/// Reads ratings from an index file with positional reads; nothing is loaded into memory.
pub struct IndexReader {
    file: File,
    count: u64,
}

impl IndexReader {
    pub fn open(path: &Path) -> io::Result<IndexReader> {
        let file = File::open(path)?;
        let mut h = [0u8; HEADER_SIZE as usize];
        read_exact_at(&file, &mut h, 0)?;
        if h[0..4] != MAGIC || u16::from_le_bytes([h[4], h[5]]) != FORMAT_VERSION {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a RandoMovie ratings index",
            ));
        }
        let count = u64::from(u32::from_le_bytes([h[8], h[9], h[10], h[11]]));
        if file.metadata()?.len() != HEADER_SIZE + count * RECORD_SIZE as u64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "ratings index has the wrong length",
            ));
        }
        Ok(IndexReader { file, count })
    }

    pub fn len(&self) -> u64 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Binary search: single-record probes until the window is small, then one read.
    pub fn get(&self, id: u32) -> io::Result<Option<Rating>> {
        let (mut lo, mut hi) = (0u64, self.count);
        let mut record = [0u8; RECORD_SIZE];
        while hi - lo > WINDOW_RECORDS {
            let mid = lo + (hi - lo) / 2;
            read_exact_at(&self.file, &mut record, offset_of(mid))?;
            match record_id(&record).cmp(&id) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Ok(Some(record_rating(&record))),
            }
        }
        let mut window = [0u8; RECORD_SIZE * WINDOW_RECORDS as usize];
        let window = &mut window[..(hi - lo) as usize * RECORD_SIZE];
        read_exact_at(&self.file, window, offset_of(lo))?;
        let (records, _) = window.as_chunks::<RECORD_SIZE>();
        Ok(records
            .binary_search_by_key(&id, |r| record_id(r))
            .ok()
            .map(|i| record_rating(&records[i])))
    }
}

fn offset_of(index: u64) -> u64 {
    HEADER_SIZE + index * RECORD_SIZE as u64
}

fn read_exact_at(file: &File, mut buf: &mut [u8], mut offset: u64) -> io::Result<()> {
    while !buf.is_empty() {
        match read_at(file, buf, offset) {
            Ok(0) => {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "ratings index ended early",
                ));
            }
            Ok(n) => {
                let rest = std::mem::take(&mut buf);
                buf = &mut rest[n..];
                offset += n as u64;
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::windows::fs::FileExt;
    file.seek_read(buf, offset)
}

#[cfg(unix)]
fn read_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<usize> {
    use std::os::unix::fs::FileExt;
    file.read_at(buf, offset)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("randomovie-{name}-{}", fastrand::u64(..)));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Rows in the dataset's text order, mixing 7- and 8-digit ids.
    const SAMPLE: &str = "tconst\taverageRating\tnumVotes\n\
        tt0000001\t5.7\t2190\n\
        tt0133093\t8.7\t2200000\n\
        tt10000000\t6.1\t12\n\
        tt1000001\t7.0\t450\n\
        tt7678620\t9.3\t21000\n\
        tt9999999\t10.0\t5\n\
        tt10872600\t8.2\t900000\n";

    #[test]
    fn parses_rows() {
        assert_eq!(parse_tconst("tt0133093"), Some(133_093));
        assert_eq!(parse_tconst("tt10872600"), Some(10_872_600));
        assert_eq!(parse_tconst("nm0000001"), None);
        assert_eq!(parse_tconst("tt"), None);
        assert_eq!(
            parse_row(b"tt0133093\t8.7\t2200000\r"),
            Some((133_093, Rating { rating_x10: 87, votes: 2_200_000 }))
        );
        assert_eq!(parse_row(b"tt0000002\t10\t5").unwrap().1.rating_x10, 100);
        assert_eq!(parse_row(b"tconst\taverageRating\tnumVotes"), None);
        assert_eq!(parse_row(b"tt0000003\t10.5\t5"), None, "rating above 10");
        assert_eq!(parse_row(b"tt0000003\t7.1\tmany"), None);
        assert_eq!(parse_row(b"tt0000003\t7.1\t5\textra"), None);
    }

    fn build(data: &[u8], chunk: usize, write_size: usize) -> (PathBuf, IndexReader, u64) {
        let dir = temp_dir("index");
        let mut builder = IndexBuilder::with_chunk_records(&dir.join("work"), chunk).unwrap();
        for piece in data.chunks(write_size) {
            builder.write_all(piece).unwrap();
        }
        assert_eq!(builder.skipped_rows(), 0);
        let out = dir.join("ratings.idx");
        let count = builder.finish(&out).unwrap();
        assert!(!dir.join("work").join("ratings.runs.tmp").exists());
        (dir, IndexReader::open(&out).unwrap(), count)
    }

    #[test]
    fn builds_sorted_index_and_looks_up() {
        // Chunks of 2 records force four runs; 3-byte writes split lines and fields.
        for (chunk, write_size) in [(2, 3), (1000, 4096), (1, 1)] {
            let (dir, reader, count) = build(SAMPLE.as_bytes(), chunk, write_size);
            assert_eq!(count, 7);
            assert_eq!(reader.len(), 7);
            assert_eq!(
                reader.get(133_093).unwrap(),
                Some(Rating { rating_x10: 87, votes: 2_200_000 })
            );
            assert_eq!(reader.get(10_000_000).unwrap().unwrap().votes, 12);
            assert_eq!(reader.get(1_000_001).unwrap().unwrap().rating_x10, 70);
            assert_eq!(reader.get(9_999_999).unwrap().unwrap().rating_x10, 100);
            assert_eq!(reader.get(1).unwrap().unwrap().votes, 2190);
            assert_eq!(reader.get(10_872_600).unwrap().unwrap().rating_x10, 82);
            assert_eq!(reader.get(2).unwrap(), None);
            assert_eq!(reader.get(u32::MAX).unwrap(), None);
            drop(reader);
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn large_index_binary_search_and_duplicates() {
        // 20 000 ids in scrambled text order, plus one duplicate, across many runs.
        let mut ids: Vec<u32> = (0..20_000u32).map(|i| i * 7 + 3).collect();
        let mut rng = fastrand::Rng::with_seed(7);
        rng.shuffle(&mut ids);
        let mut data = String::from("tconst\taverageRating\tnumVotes\n");
        for &id in &ids {
            data.push_str(&format!("tt{id:07}\t{}.{}\t{}\n", id % 10, id % 7, id));
        }
        data.push_str(&format!("tt{:07}\t1.0\t1\n", ids[0]));
        let (dir, reader, count) = build(data.as_bytes(), 1500, 8192);
        assert_eq!(count, 20_000);
        for &id in ids.iter().step_by(97) {
            let rating = reader.get(id).unwrap().expect("present");
            assert_eq!(rating.votes, id);
            assert_eq!(rating.rating_x10, ((id % 10) * 10 + id % 7) as u8);
        }
        assert_eq!(reader.get(4).unwrap(), None, "between stored ids");
        drop(reader);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_corrupt_files() {
        let dir = temp_dir("corrupt");
        let path = dir.join("bad.idx");
        fs::write(&path, b"not an index at all").unwrap();
        assert!(IndexReader::open(&path).is_err());
        let mut truncated = header(5).to_vec();
        truncated.extend_from_slice(&[0u8; RECORD_SIZE * 2]);
        fs::write(&path, truncated).unwrap();
        assert!(IndexReader::open(&path).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}
