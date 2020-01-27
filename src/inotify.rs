use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::Stream;
use inotify::{EventMask, EventOwned, EventStream, Inotify, WatchDescriptor, WatchMask};

use super::{FileSystemEvent, StopReason};

pub struct FileSystemWatcherInotify {
    root_dir: OsString,
    inotify: Inotify,
    stream: Pin<Box<EventStream<InotifyBuffer>>>,
    new_directories: VecDeque<OsString>,
    watches_by_path: BTreeMap<OsString, WatchDescriptor>,
    paths_by_watch: HashMap<WatchDescriptor, OsString>,
    removed_watches: HashSet<WatchDescriptor>,
}

impl FileSystemWatcherInotify {
    pub fn new(path: &OsStr) -> Result<FileSystemWatcherInotify, super::Error> {
        let mut inotify = Inotify::init()?;
        let stream = inotify.event_stream(InotifyBuffer { data: [0; 1024] })?;

        Ok(FileSystemWatcherInotify {
            root_dir: path.to_owned(),
            inotify,
            stream: Box::pin(stream),
            new_directories: vec![path.to_owned()].into(),
            watches_by_path: BTreeMap::new(),
            paths_by_watch: HashMap::new(),
            removed_watches: HashSet::new(),
        })
    }

    fn translate_inotify_event(&mut self, inotify_event: EventOwned) -> Option<FileSystemEvent> {
        // Ignore events for watches which are not used anymore but have not yet been removed. This
        // situation can happen if we remove a watch but there are still events buffered by the
        // inotify crate.
        if self.removed_watches.contains(&inotify_event.wd) {
            if inotify_event.mask == EventMask::IGNORED {
                // We need to remove the watch from the list, otherwise really_delete_watches()
                // will fail.
                self.removed_watches.remove(&inotify_event.wd);
            }
            return None;
        }
        if !self.paths_by_watch.contains_key(&inotify_event.wd) {
            // This should never happen.
            eprintln!("Huh, event for unknown inotify watch received?");
            return None;
        }

        let directory = self.paths_by_watch.get(&inotify_event.wd).unwrap();
        let mut path = directory.clone();
        let mut name_available = false;
        if let Some(name) = inotify_event.name.as_ref() {
            path.push("/");
            path.push(name);
            name_available = true;
        }

        // Translate the events. Note how we do not try to combine MOVED_FROM and MOVED_TO here.
        // Per the man-page, there can be arbitrary numbers of other events inbetween, so combining
        // the events is only possible if we wait some time. We could potentially reduce the CPU
        // and I/O load caused by deleting and reestablishing all the watches for the
        // subdirectories of a moved directory, but the code would become considerably more
        // complex.
        println!("{}, {:?}", path.to_string_lossy(), inotify_event);
        if inotify_event.mask == EventMask::CREATE && name_available {
            Some(FileSystemEvent::FileCreated(path))
        } else if inotify_event.mask == EventMask::MODIFY && name_available {
            Some(FileSystemEvent::FileModified(path))
        } else if inotify_event.mask == EventMask::ATTRIB && name_available {
            // TODO: Do we want a separate event type for this event?
            Some(FileSystemEvent::FileModified(path))
        } else if inotify_event.mask == EventMask::DELETE && name_available {
            Some(FileSystemEvent::FileRemoved(path))
        } else if inotify_event.mask == EventMask::MOVED_FROM && name_available {
            // TODO: Store the move cookie in the event so that it can later be combined with the
            // MOVED_TO event.
            Some(FileSystemEvent::FileRemoved(path))
        } else if inotify_event.mask == EventMask::MOVED_TO && name_available {
            // TODO: Store the move cookie in the event so that it can later be combined with the
            // MOVED_TO event.
            Some(FileSystemEvent::FileCreated(path))
        } else if inotify_event.mask == EventMask::CREATE | EventMask::ISDIR && name_available {
            // Start monitoring the directory as well.
            // We do not generate events for existing contents of the directory - the caller just
            // is notified that we started monitoring the directory and has to detect changes
            // themselves. The same logic is already required during initialization.
            self.new_directories.push_back(path.clone());
            Some(FileSystemEvent::DirectoryCreated(path))
        } else if inotify_event.mask == EventMask::DELETE | EventMask::ISDIR && name_available {
            self.delete_watches(&path);
            Some(FileSystemEvent::DirectoryRemoved(path))
        } else if inotify_event.mask == EventMask::MOVED_FROM | EventMask::ISDIR && name_available {
            self.delete_watches(&path);
            // TODO: Store the move cookie in the event so that it can later be combined with the
            // MOVED_TO event.
            Some(FileSystemEvent::DirectoryRemoved(path))
        } else if inotify_event.mask == EventMask::MOVED_TO | EventMask::ISDIR && name_available {
            // Start monitoring the directory as well.
            // We do not generate events for existing contents of the directory - the caller just
            // is notified that we started monitoring the directory and has to detect changes
            // themselves. The same logic is already required during initialization.
            self.new_directories.push_back(path.clone());
            // TODO: Store the move cookie in the event so that it can later be combined with the
            // MOVED_TO event.
            Some(FileSystemEvent::DirectoryCreated(path))
        } else if inotify_event.mask == EventMask::DELETE_SELF {
            // If this event is not about the root directory, we already generated an event for it
            // when we received DELETE. Else, notify the user that the root directory was deleted
            // and no further events will be received.
            if path == self.root_dir {
                self.delete_watches(&path);
                Some(FileSystemEvent::Stopped(StopReason::DirectoryRemoved))
            } else {
                None
            }
        } else if inotify_event.mask == EventMask::IGNORED {
            // We already received an event about the directory being deleted - we should never get
            // this event here.
            eprintln!(
                "IGNORED received ({}), but no directory removal event!",
                path.to_string_lossy()
            );
            // TODO: How to handle this situation?
            None
        } else {
            eprintln!(
                "Warning: Unexpected inotify event: {}, {:?}",
                directory.to_string_lossy(),
                inotify_event
            );
            // TODO
            None
        }
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

    fn delete_watches(&mut self, path: &OsStr) {
        // Move watches for the directory and for all subdirectories to the list of watches to be
        // deleted when the buffer has been cleared. We do not immediately delete the watches as
        // there  might still be events in the buffer of the inotify crate, so there might be some
        // races where a new watch reuses a watch ID and events are misattributed.
        let mut watches_to_delete = Vec::new();
        let mut path_iter = self.watches_by_path.range(path.to_owned()..);
        loop {
            match path_iter.next() {
                None => break,
                Some((p, wd)) => {
                    let p_path: &Path = p.as_ref();
                    if p_path.starts_with(path) {
                        watches_to_delete.push((p.clone(), wd.clone()));
                    } else {
                        // The paths are sorted, so the first entry that does not start with the
                        // path passed to this function limits the subdirectories.
                        break;
                    }
                }
            }
        }
        for (p, wd) in watches_to_delete.into_iter() {
            println!("Will delete {}/{:?}", p.to_string_lossy(), wd);
            self.watches_by_path.remove(&p);
            self.paths_by_watch.remove(&wd);
            self.removed_watches.insert(wd);
        }
        // We do not have to delete the entry from new_directories, as new watches are installed
        // before the next time inotify events are fetched, so at this point there cannot be
        // any entry in new_directories.
    }

    fn really_delete_watches(&mut self) {
        // Note that most watches are probably deleted at the top of translate_inotify_event()
        // already - however, if the directory was just moved, some watches might still be valid.
        // TODO: Check whether this is racy, and what happens then directories are concurrently
        // deleted.
        for wd in self.removed_watches.iter().cloned() {
            println!("rm_watch {:?}", wd);
            self.inotify
                .rm_watch(wd)
                .expect("could not remove inotify watch");
        }
        self.removed_watches.clear();
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
                    Poll::Pending => {
                        // The buffer has been cleared, so no we can delete watches without having
                        // to fear race conditions due to the reuse of watch IDs.
                        self_.really_delete_watches();
                        return Poll::Pending;
                    }
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
