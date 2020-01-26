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
    pub async fn new(path: &str) -> Result<FileSystemWatcher, Error> {
        #[cfg(target_os = "linux")]
        let watcher = Box::pin(FileSystemWatcherInotify::new(path).await?);

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

pub enum FileSystemEvent {
    Stopped(StopReason),
    DirectoryWatched(String),
    /// A directory was created. Note that the directory does not need to be
    /// empty - the caller has to check for existing file contents. Existing
    /// subdirectories are automatically monitored for changes.
    DirectoryCreated(String),
    DirectoryChanged(String),
    DirectoryRemoved(String),
    DirectoryMoved(String, String),
    FileCreated(String),
    FileChanged(String),
    FileRemoved(String),
    FileMoved(String, String),
    Error(Error),
}

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
