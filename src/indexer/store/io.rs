//! Positional file I/O (no shared cursor, so readers need no lock among themselves).

use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
};

#[cfg(windows)]
pub(super) fn read_at(f: &File, buf: &mut [u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = f.seek_read(&mut buf[done..], off)?;
        if n == 0 {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        done += n;
        off += n as u64;
    }
    Ok(())
}
#[cfg(windows)]
pub(super) fn write_at(f: &File, buf: &[u8], mut off: u64) -> io::Result<()> {
    use std::os::windows::fs::FileExt;
    let mut done = 0;
    while done < buf.len() {
        let n = f.seek_write(&buf[done..], off)?;
        if n == 0 {
            return Err(io::ErrorKind::WriteZero.into());
        }
        done += n;
        off += n as u64;
    }
    Ok(())
}
#[cfg(unix)]
pub(super) fn read_at(f: &File, buf: &mut [u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.read_exact_at(buf, off)
}
#[cfg(unix)]
pub(super) fn write_at(f: &File, buf: &[u8], off: u64) -> io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, off)
}

pub(super) fn open_file(path: &Path) -> io::Result<File> {
    let mut o = OpenOptions::new();
    o.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_SHARE_READ only: a second running instance cannot open the file for writing
        // and falls back to RAM-only mode, like the database did.
        o.share_mode(1);
    }
    o.open(path)
}
