# fswatcher-rs

fswatcher-rs is a library which (recursively) monitors a directory for changes.
The library has been designed to enable applications to consistently track file
system changes. In particular, this means that the library informs the
application when it starts monitoring a directory, so that during application
startup no file system changes are lost if they happen before full
initialization of the library.

The library is designed to work with tokio and offers a platform-independent
API. Currently, only Linux and inotify are supported.

## License

fswatcher-rs is licensed under the 2-clause BSD license. For more information,
see [LICENSE.md](LICENSE.md).
