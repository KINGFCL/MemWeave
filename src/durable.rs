//! 落盘：写成功就是真的写到了盘上，断电也不会留下半个文件。文件只有本人可读，记忆里可能有隐私。

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;

/// 打开文件用的选项：新建时只有本人可读写。
pub fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options
}

/// 新建目录，已存在时什么也不做。新建的目录只有本人能进。
pub fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// 写文件，不等落盘。之后要用 [`sync_tree`] 一起落盘。
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = private_options()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)?;
    file.write_all(bytes)
}

/// 让目录里新建或改名的项落盘。只落盘文件内容的话，断电后文件名可能还是会丢。
pub fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let dir = if dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            dir
        };
        File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

/// 完整写入一个文件：先写临时文件并落盘，再改名。任何时候断电，文件要么是旧的，要么是完整的新文件。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    let result = (|| {
        let mut file = private_options()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        sync_dir(path.parent().unwrap_or(Path::new("")))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 和 [`write_atomic`] 一样整个替换，但不等落盘。进程崩溃不会留下半个文件，断电可能会。
pub fn write_replace(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let temporary = path.with_extension("tmp");
    let result = write_private(&temporary, bytes).and_then(|()| fs::rename(&temporary, path));
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// 落盘时同时用多少个线程。
const SYNC_THREADS: usize = 16;

#[cfg(target_os = "linux")]
unsafe extern "C" {
    fn syncfs(fd: std::os::raw::c_int) -> std::os::raw::c_int;
}

/// 让目录树里的所有文件和目录落盘。
///
/// 一个一个来太慢，开多少线程都一样，因为文件系统的日志是串行提交的：10 万个文件要 35 秒，而这期间
/// 所有调用都在排队。Linux 上用 `syncfs` 一次落盘整个文件系统，其他系统才逐个文件来。
pub fn sync_tree(root: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;

        let dir = File::open(root)?;
        // 这个调用只用到一个打开着的文件描述符。
        if unsafe { syncfs(dir.as_raw_fd()) } == 0 {
            return Ok(());
        }
    }
    let mut files: Vec<PathBuf> = Vec::new();
    let mut dirs = vec![root.to_path_buf()];
    let mut index = 0;
    while index < dirs.len() {
        for entry in fs::read_dir(&dirs[index])? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                dirs.push(entry.path());
            } else {
                files.push(entry.path());
            }
        }
        index += 1;
    }
    let chunk = files.len().div_ceil(SYNC_THREADS).max(1);
    thread::scope(|scope| {
        let workers: Vec<_> = files
            .chunks(chunk)
            .map(|paths| {
                scope.spawn(move || {
                    for path in paths {
                        File::open(path)?.sync_all()?;
                    }
                    Ok::<(), io::Error>(())
                })
            })
            .collect();
        for worker in workers {
            worker.join().expect("落盘的线程不会崩溃")?;
        }
        Ok::<(), io::Error>(())
    })?;
    // 先落盘子目录，再落盘父目录。
    for dir in dirs.iter().rev() {
        sync_dir(dir)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn atomic_writes_replace_the_whole_file_and_leave_nothing_behind() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("a.json");

        write_atomic(&path, b"first").unwrap();
        write_atomic(&path, b"second").unwrap();

        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn sync_tree_handles_nested_and_empty_directories() {
        let dir = TempDir::new().unwrap();
        create_private_dir(&dir.path().join("a/b")).unwrap();
        for index in 0..100 {
            write_private(&dir.path().join(format!("a/{index}")), b"x").unwrap();
        }
        sync_tree(dir.path()).unwrap();
        let empty = TempDir::new().unwrap();
        sync_tree(empty.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let file = dir.path().join("a");
        let nested = dir.path().join("b/c");
        write_atomic(&file, b"x").unwrap();
        create_private_dir(&nested).unwrap();

        let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(&nested), 0o700);
    }
}
