//! Framed compressed compiled blocks. A finished footer distinguishes a complete
//! cache from a file truncated exactly on a block boundary. No raw JSONL spool.
use super::*;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{
    fs::{File, OpenOptions},
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    marker::PhantomData,
    path::Path,
};
const MAGIC: &[u8; 8] = b"BBCBLK01";
const DONE: &[u8; 8] = b"COMPLETE";
const MAX_FRAME: u64 = 512 * 1024 * 1024;
#[derive(Serialize, Deserialize)]
struct DiskBlock<C> {
    records: Vec<(usize, CaptureRecord)>,
    events: crate::simulation::StoredEvents<C>,
    config: crate::simulation::SimulationConfig,
    time_model: crate::paper_execution::PaperTimeModel,
}
#[derive(Debug)]
pub struct CompiledBlockStoreStats {
    pub blocks: usize,
    pub records: usize,
    pub bytes: u64,
}
/// Write a new file only. `identity` must include input, compilation settings and
/// converter/build identity. Use a temporary destination then rename to publish.
/// Failed writes are removed; existing files are never overwritten.
pub fn write_compiled_blocks<C: Clone + Serialize>(
    path: impl AsRef<Path>,
    identity: &str,
    source: impl IntoIterator<Item = io::Result<Arc<CompiledBlock<C>>>>,
) -> io::Result<CompiledBlockStoreStats> {
    if identity.is_empty() {
        return Err(invalid("compiled block store identity must not be empty"));
    }
    let path = path.as_ref();
    let file = OpenOptions::new().write(true).create_new(true).open(path)?;
    let result = (|| {
        let mut out = BufWriter::new(file);
        out.write_all(MAGIC)?;
        write_frame(&mut out, identity.as_bytes())?;
        let mut blocks = 0;
        let mut records = 0;
        for block in source {
            let block = block?;
            let wire = DiskBlock {
                records: block.records.clone(),
                events: block.events.store().map_err(invalid)?,
                config: block.config,
                time_model: block.time_model,
            };
            let raw = serde_json::to_vec(&wire).map_err(invalid)?;
            if raw.len() as u64 > MAX_FRAME {
                return Err(invalid(
                    "compiled block exceeds 512 MiB; reduce records_per_block",
                ));
            }
            let mut encoder = zstd::stream::Encoder::new(Vec::new(), 1)?;
            encoder.include_checksum(true)?;
            encoder.write_all(&raw)?;
            let compressed = encoder.finish()?;
            write_frame(&mut out, &compressed)?;
            blocks += 1;
            records += block.len();
        }
        out.write_all(&0_u64.to_le_bytes())?;
        out.write_all(DONE)?;
        out.flush()?;
        out.get_ref().sync_all()?;
        Ok(CompiledBlockStoreStats {
            blocks,
            records,
            bytes: out.get_ref().metadata()?.len(),
        })
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(path);
    }
    result
}
fn write_frame(out: &mut impl Write, data: &[u8]) -> io::Result<()> {
    if data.len() as u64 > MAX_FRAME {
        return Err(invalid("compiled block frame exceeds 512 MiB"));
    }
    out.write_all(&(data.len() as u64).to_le_bytes())?;
    out.write_all(data)
}
fn read_frame(input: &mut impl Read) -> io::Result<Option<Vec<u8>>> {
    let mut size = [0; 8];
    input.read_exact(&mut size)?;
    let size = u64::from_le_bytes(size);
    if size == 0 {
        return Ok(None);
    }
    if size > MAX_FRAME {
        return Err(invalid("compiled block frame exceeds 512 MiB"));
    }
    let mut data = vec![0; size as usize];
    input.read_exact(&mut data)?;
    Ok(Some(data))
}
/// Reads and reconstructs one compiled block at a time. Drop each block after
/// consumption to keep replay independent of total cache size. Decoder and raw
/// market conversion are never called. Each iteration starts with a fresh reader.
pub struct CompiledBlockReader<C> {
    input: BufReader<File>,
    ended: bool,
    _event: PhantomData<C>,
}
impl<C> CompiledBlockReader<C> {
    pub fn open(path: impl AsRef<Path>, identity: &str) -> io::Result<Self> {
        let mut input = BufReader::new(File::open(path)?);
        input.seek(SeekFrom::End(-16))?;
        let mut footer = [0; 16];
        input.read_exact(&mut footer)?;
        if footer[..8] != [0; 8] || &footer[8..] != DONE {
            return Err(invalid("unfinished compiled block store"));
        }
        input.seek(SeekFrom::Start(0))?;
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("unsupported compiled block store version"));
        }
        if read_frame(&mut input)?.as_deref() != Some(identity.as_bytes()) {
            return Err(invalid("compiled block store identity mismatch"));
        }
        Ok(Self {
            input,
            ended: false,
            _event: PhantomData,
        })
    }
}
impl<C: Clone + Send + DeserializeOwned + 'static> Iterator for CompiledBlockReader<C> {
    type Item = io::Result<Arc<CompiledBlock<C>>>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.ended {
            return None;
        }
        let result = (|| {
            let Some(frame) = read_frame(&mut self.input)? else {
                let mut done = [0; 8];
                self.input.read_exact(&mut done)?;
                if &done != DONE || self.input.read(&mut [0; 1])? != 0 {
                    return Err(invalid("invalid compiled block store footer"));
                }
                return Ok(None);
            };
            let decoder = zstd::stream::Decoder::new(frame.as_slice())?;
            let mut raw = Vec::new();
            decoder.take(MAX_FRAME + 1).read_to_end(&mut raw)?;
            if raw.len() as u64 > MAX_FRAME {
                return Err(invalid("decompressed compiled block exceeds 512 MiB"));
            }
            let wire: DiskBlock<C> = serde_json::from_slice(&raw).map_err(invalid)?;
            let events = wire.events.restore().map_err(invalid)?;
            if wire.records.len() != events.admissions.len() {
                return Err(invalid("compiled block record/admission count mismatch"));
            }
            Ok(Some(Arc::new(CompiledBlock {
                records: wire.records,
                events,
                config: wire.config,
                time_model: wire.time_model,
                apply: |b, i, d| {
                    b.events.admissions[i]
                        .as_ref()
                        .map(|a| d.admit_prepared(a, &b.events))
                },
            })))
        })();
        match result {
            Ok(Some(block)) => Some(Ok(block)),
            Ok(None) => {
                self.ended = true;
                None
            }
            Err(e) => {
                self.ended = true;
                Some(Err(e))
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip_identity_and_truncation() {
        let path =
            std::env::temp_dir().join(format!("bb-compiled-blocks-{}.bin", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let input = || {
            ReplayInput::open(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/fixtures/shared-replay/manifest.json"
            ))
            .unwrap()
        };
        let compiler = || {
            BlockCompiler::new(
                input(),
                crate::simulation::SimulationConfig { lookahead_ms: 30 },
                Default::default(),
                |_| Ok::<Option<()>, io::Error>(None),
                BlockOptions {
                    records_per_block: 3,
                    max_pending_records: 65536,
                },
            )
            .unwrap()
        };
        let expected: Vec<_> = compiler()
            .map(|b| b.unwrap().events.compiled_signature())
            .collect();
        let stats = write_compiled_blocks(&path, "input/settings/build", compiler()).unwrap();
        assert!(stats.records > 0);
        assert!(write_compiled_blocks(&path, "input/settings/build", compiler()).is_err());
        assert!(CompiledBlockReader::<()>::open(&path, "wrong input").is_err());
        let actual: Vec<_> = CompiledBlockReader::<()>::open(&path, "input/settings/build")
            .unwrap()
            .map(|b| b.unwrap().events.compiled_signature())
            .collect();
        assert_eq!(actual, expected);
        let mut bytes = std::fs::read(&path).unwrap();
        let at = bytes.len() - 17;
        bytes[at] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        assert!(
            CompiledBlockReader::<()>::open(&path, "input/settings/build")
                .unwrap()
                .collect::<io::Result<Vec<_>>>()
                .is_err()
        );
        bytes[at] ^= 1;
        std::fs::write(&path, &bytes).unwrap();
        let broken = path.with_extension("failed");
        let _ = std::fs::remove_file(&broken);
        let error = std::iter::once(Err::<Arc<CompiledBlock<()>>, _>(invalid("bad source")));
        assert!(write_compiled_blocks(&broken, "input/settings/build", error).is_err());
        assert!(!broken.exists());
        let file = OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(stats.bytes - 16).unwrap();
        drop(file);
        assert!(CompiledBlockReader::<()>::open(&path, "input/settings/build").is_err());
        std::fs::remove_file(path).unwrap();
    }
}
