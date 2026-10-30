//! A producer actor fills buffers in its own heap and sends them, without
//! copying, to a consumer on another worker. The consumer drops them, and the
//! blocks go back to the producer's heap, which reuses them.
use std::sync::mpsc;

use pony_actors::{Actor, Addr, Context, Iso, System, Val, pool};

type Buf = Iso<Vec<u64>>;

enum ProducerMsg {
    /// Make `n` buffers and send them to the consumer.
    Produce(usize),
    /// Report this actor's heap.
    Report(mpsc::Sender<String>),
}

struct Producer {
    consumer: Addr<ConsumerMsg>,
    /// Shared, immutable settings, allocated once in this actor's heap.
    config: Val<String>,
}

impl Actor for Producer {
    type Msg = ProducerMsg;
    fn receive(&mut self, cx: &Context<'_, ProducerMsg>, msg: ProducerMsg) {
        match msg {
            ProducerMsg::Produce(n) => {
                for i in 0..n as u64 {
                    // A boxed Vec header in this actor's heap; the elements
                    // (4 KiB) are a large block from the pool.
                    let mut buf = Iso::new_in(Vec::with_capacity(512), cx.heap());
                    buf.extend((0..512).map(|x| x * i));
                    self.consumer
                        .send(ConsumerMsg::Data(buf, self.config.clone()));
                }
            }
            ProducerMsg::Report(reply) => {
                let s = cx.heap().stats();
                reply
                    .send(format!(
                        "producer heap: {} live bytes, {} chunk(s), {} remote frees so far",
                        s.live_bytes, s.chunks, s.remote_frees
                    ))
                    .unwrap();
            }
        }
    }
}

enum ConsumerMsg {
    Data(Buf, Val<String>),
    Done(mpsc::Sender<(u64, usize)>),
}

struct Consumer {
    total: u64,
    buffers: usize,
    first_addr: Option<usize>,
    reused: usize,
}

impl Actor for Consumer {
    type Msg = ConsumerMsg;
    fn receive(&mut self, _cx: &Context<'_, ConsumerMsg>, msg: ConsumerMsg) {
        match msg {
            ConsumerMsg::Data(buf, config) => {
                assert_eq!(*config, "sum");
                self.total += buf.iter().sum::<u64>();
                self.buffers += 1;
                let addr = buf.as_ptr().addr();
                match self.first_addr {
                    None => self.first_addr = Some(addr),
                    Some(a) if a == addr => self.reused += 1,
                    Some(_) => {}
                }
                // `buf` drops here: a remote free back to the producer's heap.
            }
            ConsumerMsg::Done(reply) => reply.send((self.total, self.reused)).unwrap(),
        }
    }
}

fn main() {
    let system = System::new(2);
    let consumer = system.spawn(|_| Consumer {
        total: 0,
        buffers: 0,
        first_addr: None,
        reused: 0,
    });
    let producer = system.spawn({
        let consumer = consumer.clone();
        move |heap| Producer {
            consumer,
            config: Val::new_in("sum".to_string(), heap),
        }
    });

    let (report_tx, report_rx) = mpsc::channel();
    let (done_tx, done_rx) = mpsc::channel();
    for round in 1..=3 {
        producer.send(ProducerMsg::Produce(1000));
        producer.send(ProducerMsg::Report(report_tx.clone()));
        println!("round {round}: {}", report_rx.recv().unwrap());
    }
    consumer.send(ConsumerMsg::Done(done_tx));
    let (total, reused) = done_rx.recv().unwrap();
    println!("consumer: sum = {total}");
    println!("consumer: the first buffer's address came back {reused} more times");

    let stats = system.shutdown();
    for (i, s) in stats.iter().enumerate() {
        println!(
            "worker {i}: {} messages in {} turns, {} blocks collected back into heaps",
            s.messages, s.turns, s.collected
        );
    }
    let p = pool::stats();
    println!(
        "pool: {} slabs over {} threads, {} batches of foreign frees sent",
        p.slabs, p.threads, p.batches_sent
    );
}
