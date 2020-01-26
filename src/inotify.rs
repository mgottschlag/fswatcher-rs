use std::collections::{BTreeMap, HashMap, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use inotify::{EventOwned, EventStream, Inotify, WatchDescriptor, WatchMask};

use super::FileSystemEvent;

pub struct FileSystemWatcherInotify {
    inotify: Inotify,
    stream: Pin<Box<EventStream<InotifyBuffer>>>,
    new_directories: VecDeque<OsString>,
    watches_by_path: BTreeMap<OsString, WatchDescriptor>,
    paths_by_watch: HashMap<WatchDescriptor, OsString>,
}

impl FileSystemWatcherInotify {
    pub async fn new(path: &OsStr) -> Result<FileSystemWatcherInotify, super::Error> {
        // TODO: Better error handling.
        let mut inotify = Inotify::init().expect("Failed to initialize inotify");
        let stream = inotify
            .event_stream(InotifyBuffer { data: [0; 1024] })
            .unwrap();

        Ok(FileSystemWatcherInotify {
            inotify,
            stream: Box::pin(stream),
            new_directories: vec![path.to_owned()].into(),
            watches_by_path: BTreeMap::new(),
            paths_by_watch: HashMap::new(),
        })
    }

    fn translate_inotify_event(&mut self, inotify_event: EventOwned) -> Option<FileSystemEvent> {
        println!("Inotify event: {:?}", inotify_event);
        // TODO
        None
    }

    fn watch_subdirectories(&mut self, path: &OsStr) {
        match fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => {
                            match entry.file_type() {
                                Ok(file_type) => {
                                    if file_type.is_dir() {
                                        let new_dir =
                                            entry.path().as_path().as_os_str().to_os_string();
                                        self.new_directories.push_back(new_dir);
                                    }
                                }
                                Err(e) => eprintln!(
                                    "Warning: Cannot determine file type of {}: {:?}",
                                    entry.path().as_path().to_str().unwrap_or("(non-UTF path)"),
                                    e
                                ),
                            };
                        }
                        Err(e) => eprintln!(
                            "Warning: Error during directory listing of {}: {:?}",
                            path.to_string_lossy(),
                            e
                        ),
                    };
                }
            }
            Err(e) => {
                // TODO: Should we do anything here? The directory is most likely not
                // readable due to (intentionally set) access rights.
                eprintln!(
                    "Warning: Cannot monitor directory {}: {:?}",
                    path.to_string_lossy(),
                    e
                );
            }
        };
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
                match Pin::as_mut(&mut self_.stream).poll_next(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => return Poll::Ready(None),
                    Poll::Ready(Some(Ok(event))) => {
                        let translated = self_.translate_inotify_event(event);
                        if let Some(event) = translated {
                            return Poll::Ready(Some(event));
                        } else {
                            // Some inotify events do not directly translate into our events, such
                            // as MOVED_FROM/MOVED_TO. Simply try to read the next event.
                        }
                    }
                    Poll::Ready(Some(Err(e))) => {
                        return Poll::Ready(Some(FileSystemEvent::Error(e.into())))
                    }
                };
            } else {
                // Install an inotify watch for the directory and report that the directory was added.
                let new_directory = self_.new_directories.pop_front().unwrap();
                // TODO: Check whether the list of active watches already contains the directory.

                // TODO: Do not follow links!
                // TODO: Is ONLYDIR correct?
                if Path::new(&new_directory).is_dir() {
                    let watch = match self_.inotify.add_watch(
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
                    ) {
                        Ok(watch) => watch,
                        Err(e) => return Poll::Ready(Some(FileSystemEvent::Error(e.into()))),
                    };

                    // Enter the directory into the list of active watches.
                    self_
                        .watches_by_path
                        .insert(new_directory.clone(), watch.clone());
                    self_
                        .paths_by_watch
                        .insert(watch.clone(), new_directory.clone());

                    // Enter all subdirectories into the list of new directories.
                    self_.watch_subdirectories(&new_directory);

                    return Poll::Ready(Some(FileSystemEvent::DirectoryWatched(new_directory)));
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
