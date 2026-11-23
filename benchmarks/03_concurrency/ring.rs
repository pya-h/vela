use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread;

fn worker(id: i32, in_rx: Receiver<i64>, out_tx: SyncSender<i64>) -> thread::JoinHandle<i32> {
    thread::spawn(move || {
        loop {
            let token = in_rx.recv().unwrap();
            if token == 0 {
                let _ = out_tx.send(0);
                return id;
            }
            let _ = out_tx.send(token - 1);
        }
    })
}

fn main() {
    let passes: i64 = 100000;
    let (tx0, rx0) = sync_channel::<i64>(1);
    let (tx1, rx1) = sync_channel::<i64>(1);
    let (tx2, rx2) = sync_channel::<i64>(1);
    let (tx3, rx3) = sync_channel::<i64>(1);

    let h0 = worker(0, rx0, tx1);
    let h1 = worker(1, rx1, tx2);
    let h2 = worker(2, rx2, tx3);
    let h3 = worker(3, rx3, tx0.clone());

    tx0.send(passes).unwrap();

    let r0 = h0.join().unwrap();
    let r1 = h1.join().unwrap();
    let r2 = h2.join().unwrap();
    let r3 = h3.join().unwrap();

    println!("ring completed {} passes: {} {} {} {}", passes, r0, r1, r2, r3);
}
