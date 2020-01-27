use std::collections::{BTreeMap, BTreeSet, HashMap};
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
    new_directories: BTreeSet<OsString>,
    watches_by_path: BTreeMap<OsString, WatchDescriptor>,
    paths_by_watch: HashMap<WatchDescriptor, OsString>,
    //removed_watches: HashSet<WatchDescriptor>,
}

impl FileSystemWatcherInotify {
    pub fn new(path: &OsStr) -> Result<FileSystemWatcherInotify, super::Error> {
        let mut inotify = Inotify::init()?;
        let stream = inotify.event_stream(InotifyBuffer { data: [0; 1024] })?;

        let mut new_directories = BTreeSet::new();
        new_directories.insert(path.to_owned());
        Ok(FileSystemWatcherInotify {
            root_dir: path.to_owned(),
            inotify,
            stream: Box::pin(stream),
            new_directories,
            watches_by_path: BTreeMap::new(),
            paths_by_watch: HashMap::new(),
        })
    }

    fn translate_inotify_event(&mut self, inotify_event: EventOwned) -> Option<FileSystemEvent> {
        // TODO: Modify code to delete entries from new_directories as well.

        if inotify_event.mask == EventMask::IGNORED {
            // We manually deleted the watch or the directory was deleted. In any case, there is
            // nothing to do here.
            return None;
        }
        if !self.paths_by_watch.contains_key(&inotify_event.wd) {
            // We probably already deleted the watch. Ignore the event.
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
            self.new_directories.insert(path.clone());
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
            self.new_directories.insert(path.clone());
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
        } else {
            eprintln!(
                "Warning: Unexpected inotify event: {}, {:?}",
                directory.to_string_lossy(),
                inotify_event
            );
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
                                        self.new_directories.insert(new_dir);
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
        // Remove watches for the directory and for all subdirectories.
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
            self.watches_by_path.remove(&p);
            self.paths_by_watch.remove(&wd);
            // We ignore errors here, because the IGNORED event for the watch might already be in
            // the inotify buffer (meaning that the watch is already invalid), we just did not read
            // and process it yet.
            self.inotify.rm_watch(wd).ok();
        }
        // We also have to delete the entries from new_directories, as watches are only added once
        // the inotify buffer has been drained. We do not want to accidently add watches for these
        // deleted directories if the user concurrently creates a new directory.
        //
        // This has to be as complicated as is, because new_directories might contain an arbitrary
        // number of subdirectories of an (already watched) directory.
        let mut new_dirs_to_delete = Vec::new();
        let mut path_iter = self.new_directories.range(path.to_owned()..);
        loop {
            match path_iter.next() {
                None => break,
                Some(p) => {
                    let p_path: &Path = p.as_ref();
                    if p_path.starts_with(path) {
                        new_dirs_to_delete.push(p.clone());
                    } else {
                        // The paths are sorted, so the first entry that does not start with the
                        // path passed to this function limits the subdirectories.
                        break;
                    }
                }
            }
        }
        for p in new_dirs_to_delete.into_iter() {
            self.new_directories.remove(&p);
        }
    }

    fn poll_inotify_stream(&mut self, cx: &mut Context) -> Poll<Option<FileSystemEvent>> {
        loop {
            match Pin::as_mut(&mut self.stream).poll_next(cx) {
                Poll::Pending => {
                    return Poll::Pending;
                }
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Ok(event))) => {
                    let translated = self.translate_inotify_event(event);
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
        }
    }
}

impl Stream for FileSystemWatcherInotify {
    type Item = FileSystemEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        // Safe, as we will not move self_.
        let self_ = unsafe { self.get_unchecked_mut() };

        // Here, the order is important to prevent watch descriptor reuse. We must first drain
        // the inotify buffer before we can add any new watches. See
        // https://github.com/hannobraun/inotify/issues/73 for a description of the issue.

        match self_.poll_inotify_stream(cx) {
            Poll::Pending => {
                // Continue below and add any
            }
            x => return x,
        }

        loop {
            if !self_.new_directories.is_empty() {
                // Install an inotify watch for a new directory and report that the directory was added.
                let new_directory = self_.new_directories.iter().next().unwrap().clone();
                self_.new_directories.remove(&new_directory);

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
                    // We failed, but there might be more new directories. Just try again.
                    continue;
                }
            }

            // No events, no new directories.
            return Poll::Pending;
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
