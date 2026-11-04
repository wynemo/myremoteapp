//! Bounded file bundles. Paths are portable relative names; no links or metadata
//! from the sender are restored. Every receive uses a fresh private directory.
use anyhow::{Context, Result, ensure};
use remote_session::clipboard::MAX_FILES;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_ENTRIES: usize = 4096;
const MAX_MANIFEST: usize = 1024 * 1024;
const MAX_DEPTH: usize = 64;

#[derive(Serialize, Deserialize)]
struct Entry {
    path: String,
    // None is a directory, including empty directories.
    length: Option<u64>,
}

fn validate_path(path: &str) -> Result<()> {
    ensure!(path.split('/').count() <= MAX_DEPTH, "文件夹层级超限");
    for name in path.split('/') {
        ensure!(
            !name.is_empty() && name != "." && name != "..",
            "无效文件路径"
        );
        ensure!(
            name.len() <= 240 && !name.ends_with(['.', ' ']),
            "不兼容的文件名"
        );
        ensure!(
            !name
                .chars()
                .any(|c| c.is_control() || "\\:<>\"|?*".contains(c)),
            "不兼容的文件名"
        );
        let stem = name.split('.').next().unwrap().to_uppercase();
        ensure!(
            !matches!(
                stem.as_str(),
                "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
            ) && !["COM", "LPT"]
                .iter()
                .any(|prefix| stem.strip_prefix(prefix).is_some_and(|s| matches!(
                    s,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                ))),
            "不兼容的设备文件名"
        );
    }
    Ok(())
}

fn collect(
    path: &Path,
    relative: String,
    entries: &mut Vec<Entry>,
    body: &mut Vec<u8>,
) -> Result<()> {
    validate_path(&relative)?;
    ensure!(entries.len() < MAX_ENTRIES, "文件数量超限（最多 4096 项）");
    let metadata = fs::symlink_metadata(path)?;
    ensure!(!metadata.is_symlink(), "不支持符号链接");
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(metadata.file_attributes() & 0x400 == 0, "不支持重解析点");
    }
    if metadata.is_dir() {
        entries.push(Entry {
            path: relative.clone(),
            length: None,
        });
        for child in fs::read_dir(path)? {
            let child = child?;
            let name = child
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("文件名不是 UTF-8"))?;
            collect(&child.path(), format!("{relative}/{name}"), entries, body)?;
        }
    } else {
        ensure!(metadata.is_file(), "不支持特殊文件");
        ensure!(
            metadata.len() <= (MAX_FILES - body.len()) as u64,
            "文件总大小超限（64 MiB）"
        );
        let before = body.len();
        fs::File::open(path)?
            .take((MAX_FILES - before + 1) as u64)
            .read_to_end(body)?;
        ensure!(body.len() <= MAX_FILES, "文件总大小超限（64 MiB）");
        ensure!(
            body.len() - before == metadata.len() as usize,
            "复制期间文件发生变化，请重新复制"
        );
        entries.push(Entry {
            path: relative,
            length: Some(metadata.len()),
        });
    }
    Ok(())
}

pub fn pack(paths: &[PathBuf]) -> Result<Vec<u8>> {
    ensure!(
        !paths.is_empty() && paths.len() <= MAX_ENTRIES,
        "文件数量超限"
    );
    let mut entries = Vec::new();
    let mut body = Vec::new();
    for path in paths {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .context("无效文件名")?;
        collect(path, name.to_owned(), &mut entries, &mut body)?;
    }
    let manifest = serde_json::to_vec(&entries)?;
    ensure!(
        manifest.len() <= MAX_MANIFEST && 4 + manifest.len() + body.len() <= MAX_FILES,
        "文件包大小超限（64 MiB）"
    );
    let mut bundle = Vec::with_capacity(4 + manifest.len() + body.len());
    bundle.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
    bundle.extend_from_slice(&manifest);
    bundle.extend_from_slice(&body);
    // Also reject overlapping selections and case-insensitive collisions locally.
    parse(&bundle)?;
    Ok(bundle)
}

fn parse(bundle: &[u8]) -> Result<(Vec<Entry>, &[u8])> {
    ensure!(
        bundle.len() >= 4 && bundle.len() <= MAX_FILES,
        "无效文件包大小"
    );
    let length = u32::from_le_bytes(bundle[..4].try_into()?) as usize;
    ensure!(
        length <= MAX_MANIFEST && length <= bundle.len() - 4,
        "无效文件清单"
    );
    let entries: Vec<Entry> = serde_json::from_slice(&bundle[4..4 + length])?;
    ensure!(
        !entries.is_empty() && entries.len() <= MAX_ENTRIES,
        "文件数量超限"
    );
    let mut names = HashSet::new();
    let mut directories = HashSet::new();
    let mut total = 0u64;
    for entry in &entries {
        validate_path(&entry.path)?;
        let key = entry.path.to_lowercase();
        ensure!(names.insert(key.clone()), "文件名冲突");
        if let Some((parent, _)) = key.rsplit_once('/') {
            ensure!(directories.contains(parent), "文件的父目录不存在");
        }
        if let Some(size) = entry.length {
            total = total.checked_add(size).context("文件大小溢出")?;
        } else {
            directories.insert(key);
        }
    }
    let body = &bundle[4 + length..];
    ensure!(total == body.len() as u64, "文件包数据长度不匹配");
    Ok((entries, body))
}

pub struct Staged {
    pub _directory: tempfile::TempDir,
    pub roots: Vec<PathBuf>,
}

pub fn unpack(bundle: &[u8]) -> Result<Staged> {
    let (entries, mut body) = parse(bundle)?;
    let directory = tempfile::Builder::new()
        .prefix("myremoteapp-clipboard-")
        .tempdir()?;
    let mut roots = Vec::new();
    for entry in entries {
        let path = directory.path().join(&entry.path);
        if !entry.path.contains('/') {
            roots.push(path.clone());
        }
        if let Some(length) = entry.length {
            let (data, rest) = body.split_at(length as usize);
            use std::io::Write;
            fs::File::options()
                .write(true)
                .create_new(true)
                .open(path)?
                .write_all(data)?;
            body = rest;
        } else {
            fs::create_dir(path)?;
        }
    }
    Ok(Staged {
        _directory: directory,
        roots,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn multiple_files_nested_and_empty_directories_roundtrip() {
        let source = tempfile::tempdir().unwrap();
        let folder = source.path().join("中文🙂");
        fs::create_dir_all(folder.join("empty")).unwrap();
        fs::write(folder.join("binary.dat"), [0, 255, 13, 10]).unwrap();
        let file = source.path().join("zero.txt");
        fs::write(&file, []).unwrap();
        let staged = unpack(&pack(&[folder, file]).unwrap()).unwrap();
        assert_eq!(staged.roots.len(), 2);
        assert!(staged.roots[0].join("empty").is_dir());
        assert_eq!(
            fs::read(staged.roots[0].join("binary.dat")).unwrap(),
            [0, 255, 13, 10]
        );
        assert_eq!(fs::metadata(&staged.roots[1]).unwrap().len(), 0);
        let location = staged._directory.path().to_owned();
        drop(staged);
        assert!(!location.exists());
    }

    fn bundle(entries: Vec<Entry>, body: &[u8]) -> Vec<u8> {
        let manifest = serde_json::to_vec(&entries).unwrap();
        let mut bytes = (manifest.len() as u32).to_le_bytes().to_vec();
        bytes.extend(manifest);
        bytes.extend(body);
        bytes
    }

    #[test]
    fn rejects_paths_collisions_missing_parents_and_invalid_lengths() {
        for path in [
            "../escape",
            "/absolute",
            "C:/drive",
            "a\\b",
            "a//b",
            "a/./b",
            "NUL.txt",
            "COM1",
            "a.",
            "a ",
            "x:stream",
            "a\0b",
        ] {
            assert!(
                unpack(&bundle(
                    vec![Entry {
                        path: path.into(),
                        length: Some(0)
                    }],
                    &[]
                ))
                .is_err(),
                "{path}"
            );
        }
        for entries in [
            vec![Entry {
                path: "a/b".into(),
                length: Some(0),
            }],
            vec![
                Entry {
                    path: "a".into(),
                    length: Some(0),
                },
                Entry {
                    path: "A".into(),
                    length: None,
                },
            ],
            vec![Entry {
                path: "a".into(),
                length: Some(u64::MAX),
            }],
            vec![Entry {
                path: "a".into(),
                length: Some(1),
            }],
        ] {
            assert!(unpack(&bundle(entries, &[])).is_err());
        }
        assert!(unpack(&[255; 4]).is_err());
        assert!(
            unpack(&bundle(
                vec![Entry {
                    path: "a".into(),
                    length: Some(0)
                }],
                &[1]
            ))
            .is_err()
        );
    }

    #[test]
    fn rejects_large_files_and_overlapping_selections() {
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("large");
        fs::File::create(&file)
            .unwrap()
            .set_len(MAX_FILES as u64 + 1)
            .unwrap();
        assert!(pack(std::slice::from_ref(&file)).is_err());
        fs::write(&file, []).unwrap();
        assert!(pack(&[file.clone(), file]).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symbolic_links() {
        let source = tempfile::tempdir().unwrap();
        let link = source.path().join("link");
        std::os::unix::fs::symlink(source.path(), &link).unwrap();
        assert!(pack(&[link]).is_err());
    }
}
