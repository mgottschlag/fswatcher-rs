use std::collections::VecDeque;
use std::mem;
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::Duration;

use futures::stream::Stream;
use tokio::time::{self, Interval};

use super::FileSystemEvent;

/// A stream which delays file system events and combines them whenever possible.
///
/// There are several reasons why you might want to use this type:
///
/// * If a file is frequently modified, you might want to rate-limit the notifications in order to
///   reduce CPU/I/O usage.
/// * Similarly, the type hides creation and subsequent deletion of very short-lived files, as the
///   user usually has no way to successfully use those files anyways.
/// * inotify supports notifications for moved files, but the interface is inherently racy. This
///   type combines the two file system events for a moved file into the correct event. The delay
///   is required to make sure that the second event is available before the first has been passed
///   to the user of the library.
pub struct FileEventDelay<T>
where
    T: Stream<Item = FileSystemEvent>,
{
    input: Pin<Box<T>>,

    min_delay: Duration,
    timer: Option<Pin<Box<Interval>>>,

    /// File system events which are queued for processing.
    ///
    /// Whenever the timer expires, the events from the second array are processed and the content
    /// of the first array is moved to the second. This way, each event is in the queue for a
    /// duration of at least `min_delay`.
    event_queue: (Vec<FileSystemEvent>, Vec<FileSystemEvent>),
    processed_events: VecDeque<FileSystemEvent>,
}

impl<T> FileEventDelay<T>
where
    T: Stream<Item = FileSystemEvent>,
{
    pub fn new(input: T, min_delay: Duration) -> Self {
        Self {
            input: Box::pin(input),
            min_delay,
            timer: None,
            event_queue: (Vec::new(), Vec::new()),
            processed_events: VecDeque::new(),
        }
    }

    fn process_events(&mut self) {
        // TODO: Actually process and combine the events.
        for event in self.event_queue.1.drain(..) {
            self.processed_events.push_back(event);
        }

        // Swap the arrays. This code assumes that the second array is now empty.
        // TODO: Check the assumption.
        mem::swap(&mut self.event_queue.0, &mut self.event_queue.1);
    }
}

impl<T> Stream for FileEventDelay<T>
where
    T: Stream<Item = FileSystemEvent>,
{
    type Item = FileSystemEvent;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Option<Self::Item>> {
        // Safe, as we will not move self_.
        let self_ = unsafe { self.get_unchecked_mut() };

        if self_.processed_events.is_empty() {
            // Fill the first array with incoming inotify events.
            while let Poll::Ready(inotify_event) = Pin::as_mut(&mut self_.input).poll_next(cx) {
                if inotify_event.is_some() {
                    self_.event_queue.0.push(inotify_event.unwrap());
                } else {
                    // TODO: Stream exhausted. Can this ever happen?
                    return Poll::Ready(None);
                }
            }

            // If the first array is not empty, start a timer for processing if no timer is already
            // running.
            if !self_.event_queue.0.is_empty() && self_.timer.is_none() {
                self_.timer = Some(Box::pin(time::interval(self_.min_delay)));
            }

            // If the timer elapsed, process the events from the second array and move the content
            // of the first array to the second.
            if let Poll::Ready(Some(_)) =
                Pin::as_mut(&mut self_.timer.as_mut().unwrap()).poll_next(cx)
            {
                self_.process_events();

                // If no unprocessed events are available, stop the timer to reduce CPU
                // consumption.
                if self_.event_queue.1.is_empty() {
                    self_.timer = None;
                }
                // Return the first event if possible.
                if let Some(next_event) = self_.processed_events.pop_front() {
                    return Poll::Ready(Some(next_event));
                }
            }

            Poll::Pending
        } else {
            let next_event = self_.processed_events.pop_front().unwrap();
            Poll::Ready(Some(next_event))
        }
    }
}

/*pub struct NextFileEvent<'a, T> where T: Stream<Item = io::Result<inotify::EventOwned>> {
    from: &'a mut FileEventDelay<T>,
    paths_by_watch: &'a HashMap<WatchDescriptor, String>,
}

impl<'a, T> Future for NextFileEvent<'a, T> where T: Stream<Item = io::Result<inotify::EventOwned>> {
    type Output = Option<FileEvent>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        // Safe, as we will not move self_.
        let self_ = unsafe { self.get_unchecked_mut() };

        self_.from.poll_next_event(cx, self_.paths_by_watch)

    }
}*/
