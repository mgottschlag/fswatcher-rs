use std::ffi::{OsStr, OsString};
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::stream::Stream;

#[cfg(target_os = "linux")]
use crate::inotify::FileSystemWatcherInotify;
#[cfg(target_os = "linux")]
mod inotify;

pub struct FileSystemWatcher {
    #[cfg(target_os = "linux")]
    watcher: Pin<Box<FileSystemWatcherInotify>>,
}

impl FileSystemWatcher {
    pub fn new(path: &OsStr) -> Result<FileSystemWatcher, Error> {
        #[cfg(target_os = "linux")]
        let watcher = Box::pin(FileSystemWatcherInotify::new(path)?);

        #[cfg(not(target_os = "linux"))]
        panic!("Not yet implemented.");

        Ok(FileSystemWatcher { watcher })
    }
}

impl Stream for FileSystemWatcher {
    type Item = FileSystemEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        // Safe, as we will not move self_.
        let self_ = unsafe { self.get_unchecked_mut() };

        Pin::as_mut(&mut self_.watcher).poll_next(cx)
    }
}

#[derive(Debug)]
pub enum FileSystemEvent {
    Stopped(StopReason),
    DirectoryWatched(OsString),
    /// A directory was created. Note that the directory does not need to be
    /// empty - the caller has to check for existing file contents. Existing
    /// subdirectories are automatically monitored for changes.
    DirectoryCreated(OsString),
    DirectoryModified(OsString),
    DirectoryRemoved(OsString),
    DirectoryMoved(OsString, OsString),
    FileCreated(OsString),
    FileModified(OsString),
    FileRemoved(OsString),
    FileMoved(OsString, OsString),
    Error(Error),
}

#[derive(Debug)]
pub enum StopReason {
    DirectoryRemoved,
}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Error {
        Error::Io(e)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
