use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use inotify::{EventOwned, EventStream, Inotify, WatchDescriptor, WatchMask};

use super::FileSystemEvent;

pub struct FileSystemWatcherInotify {
    inotify: Inotify,
    stream: Pin<Box<EventStream<InotifyBuffer>>>,
    new_directories: VecDeque<String>,
    watches_by_path: BTreeMap<String, WatchDescriptor>,
    paths_by_watch: HashMap<WatchDescriptor, String>,
}

impl FileSystemWatcherInotify {
    pub async fn new(path: &str) -> Result<FileSystemWatcherInotify, super::Error> {
        // TODO: Better error handling.
        let mut inotify = Inotify::init().expect("Failed to initialize inotify");
        let stream = inotify
            .event_stream(InotifyBuffer { data: [0; 1024] })
            .unwrap();

        Ok(FileSystemWatcherInotify {
            inotify,
            stream: Box::pin(stream),
            new_directories: VecDeque::new(),
            watches_by_path: BTreeMap::new(),
            paths_by_watch: HashMap::new(),
        })
    }

    fn translate_inotify_event(&self, inofity_event: EventOwned) -> FileSystemEvent {
        // TODO
        panic!("Not yet implemented.");
    }
}

impl Stream for FileSystemWatcherInotify {
    type Item = FileSystemEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        // Safe, as we will not move self_.
        let self_ = unsafe { self.get_unchecked_mut() };

        loop {
            if self_.new_directories.is_empty() {
                // If all the inotify watches are up-to-date, simply wait for any events from inotify.
                // TODO: Recognize when files are moved!
                return match Pin::as_mut(&mut self_.stream).poll_next(cx) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(None) => Poll::Ready(None),
                    Poll::Ready(Some(Ok(event))) => {
                        Poll::Ready(Some(self_.translate_inotify_event(event)))
                    }
                    Poll::Ready(Some(Err(e))) => Poll::Ready(Some(FileSystemEvent::Error(e.into()))),
                };
            } else {
                // Install an inotify watch for the directory and report that the directory was added.
                let new_directory = self_.new_directories.pop_front().unwrap();
                // TODO: Check whether the list of active watches already contains the directory.

                // TODO: Do not follow links!
                // TODO: Is ONLYDIR correct?
                if Path::new(&new_directory).is_dir() {
                    let watch = self_.inotify.add_watch(
                        &new_directory,
                        WatchMask::ATTRIB
                            | WatchMask::CLOSE_WRITE
                            | WatchMask::CREATE
                            | WatchMask::DELETE
                            | WatchMask::DELETE_SELF
                            | WatchMask::MODIFY
                            | WatchMask::MOVE
                            | WatchMask::EXCL_UNLINK
                            | WatchMask::ONLYDIR,
                    );
                    // TODO
                    // TODO: Enter the directory into the list of active watches.
                    // TODO: Enter all subdirectories into the list of new directories.
                    return Poll::Ready(Some(FileSystemEvent::DirectoryWatched(new_directory)))
                } else {
                    // Just try again.
                }
            }
        }
    }
}

struct InotifyBuffer {
    data: [u8; 1024],
}

impl AsMut<[u8]> for InotifyBuffer {
    fn as_mut<'a>(&'a mut self) -> &'a mut [u8] {
        &mut self.data
    }
}

impl AsRef<[u8]> for InotifyBuffer {
    fn as_ref<'a>(&'a self) -> &'a [u8] {
        &self.data
    }
}
