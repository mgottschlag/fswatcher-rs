use std::env;
use std::ffi::OsString;
use std::process;

use futures_util::StreamExt;

use fswatcher::FileSystemWatcher;

#[tokio::main]
async fn main() {
    let args = env::args().collect::<Vec<_>>();
    if args.len() != 2 {
        eprintln!("Wrong number of parameters.");
        eprintln!("Usage: {} <path>", args[0]);
        process::exit(-1);
    }
    let path = &OsString::from(&args[1]);

    let mut fsw = FileSystemWatcher::new(path).expect("could not create filesystem watcher");

    while let Some(event) = fsw.next().await {
        println!("Event: {:?}", event);
    }
}

