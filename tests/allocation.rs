//! The unsafe implementation belongs only to this std test's allocator oracle.
//! The published core forbids unsafe Rust and does not depend on this test.
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use zconv::ir::Record;
use zconv::json::Token;
use zconv::{convert, Operation, Options, Protocol, Strategy, Workspace};

struct CountingAllocator;
thread_local! {
    // Count this test's thread, not unrelated libtest reporter activity. The
    // core cannot spawn threads. Const, non-Drop TLS needs no heap allocation.
    static COUNTERS: Cell<(bool, usize, usize)> = const { Cell::new((false, 0, 0)) };
}

fn record(bytes: usize) {
    let _ = COUNTERS.try_with(|counters| {
        let (active, count, total) = counters.get();
        if active {
            counters.set((true, count.saturating_add(1), total.saturating_add(bytes)));
        }
    });
}

// SAFETY: allocation contracts are delegated unchanged to System. The const
// Cell only records requests; it cannot allocate, alter pointers or hold a borrow
// across an allocator call. try_with safely handles teardown on other threads.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record(new_size);
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn parser_direct_ir_and_identity_allocate_zero_bytes() {
    // A positive control proves that the allocator oracle observes real calls.
    COUNTERS.with(|counters| counters.set((true, 0, 0)));
    let probe = std::hint::black_box(Vec::<u8>::with_capacity(17));
    drop(probe);
    let (_, count, bytes) = COUNTERS.with(|counters| counters.replace((false, 0, 0)));
    assert!(count > 0 && bytes >= 17);
    let input =
        br#"{"model":"m","messages":[{"role":"user","content":"hello \u4e2d\ud83d\ude00"}]}"#;
    let mut tokens = [Token::EMPTY; 128];
    let mut nested = [Token::EMPTY; 128];
    let mut records = [Record::EMPTY; 32];
    let mut output = [0; 2048];
    let mut scratch = [0; 1024];
    let mut ok = true;
    COUNTERS.with(|counters| counters.set((true, 0, 0)));
    for _ in 0..32 {
        for (target, strategy) in [
            (Protocol::OpenAiChat, Strategy::Auto),
            (Protocol::OpenAiResponses, Strategy::Direct),
            (Protocol::OpenAiResponses, Strategy::Ir),
        ] {
            let result = convert(
                Protocol::OpenAiChat.into(),
                target.into(),
                Operation::Request,
                input,
                &mut output,
                Options {
                    strategy,
                    ..Options::default()
                },
                Workspace {
                    tokens: &mut tokens,
                    records: &mut records,
                    scratch: &mut scratch,
                    nested_tokens: &mut nested,
                },
            );
            ok &= result.is_ok();
        }
    }
    let wire = b"data: {\"id\":\"r\",\"model\":\"m\",\"created\":1,\"choices\":[{\"index\":0,\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":2,\"completion_tokens\":1,\"total_tokens\":3}}\n\ndata: [DONE]\n\n";
    let mut framing = [0; 512];
    let mut data = [0; 512];
    let mut pending = [0; 2048];
    let mut items = [zconv::stream::ActiveItem::EMPTY; 4];
    let mut checkpoints = [zconv::stream::ActiveItem::EMPTY; 4];
    let mut events = [zconv::stream::Event::EMPTY; 8];
    let mut aggregate = [0; 512];
    for strategy in [Strategy::Direct, Strategy::Ir] {
        let mut config =
            zconv::stream::StreamConfig::new(Protocol::OpenAiChat, Protocol::OpenAiResponses);
        config.strategy = strategy;
        config.context.item_id_prefix = Some("local_");
        if let Ok(mut stream) = zconv::stream::StreamConverter::new(
            config,
            zconv::stream::StreamBuffers {
                framing: &mut framing,
                data: &mut data,
                pending: &mut pending,
                tokens: &mut tokens,
                items: &mut items,
                checkpoints: &mut checkpoints,
                events: &mut events,
                aggregate: &mut aggregate,
            },
        ) {
            let mut position = 0;
            let mut done = false;
            let mut fragment = [0; 13];
            for _ in 0..512 {
                let step = stream.step(&wire[position..], &mut fragment, true);
                position += step.consumed;
                match step.status {
                    zconv::stream::Status::Done => {
                        done = true;
                        break;
                    }
                    zconv::stream::Status::NeedInput | zconv::stream::Status::NeedOutput => {}
                    _ => {
                        ok = false;
                        break;
                    }
                }
            }
            ok &= done;
        } else {
            ok = false;
        }
    }
    let (_, count, bytes) = COUNTERS.with(|counters| counters.replace((false, 0, 0)));
    assert!(ok);
    assert_eq!(count, 0);
    assert_eq!(bytes, 0);
}
