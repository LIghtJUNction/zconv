//! The unsafe implementation belongs only to this std test's allocator oracle.
//! The published core forbids unsafe Rust and does not depend on this test.
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use zconv::ir::Record;
use zconv::json::Token;
use zconv::{convert, Operation, Options, Protocol, Strategy, Workspace};

struct CountingAllocator;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
static BYTES: AtomicUsize = AtomicUsize::new(0);

// SAFETY: all allocation contracts are delegated unchanged to System. Atomics
// merely record requests and cannot reenter the allocator or alter pointers.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) }
    }
    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        if ACTIVE.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            BYTES.fetch_add(new_size, Ordering::Relaxed);
        }
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

#[test]
fn parser_direct_ir_and_identity_allocate_zero_bytes() {
    let input = br#"{"model":"m","messages":[{"role":"user","content":"hello \u4e2d\ud83d\ude00"}]}"#;
    let mut tokens = [Token::EMPTY; 128];
    let mut nested = [Token::EMPTY; 128];
    let mut records = [Record::EMPTY; 32];
    let mut output = [0; 2048];
    let mut scratch = [0; 1024];
    let mut ok = true;
    ACTIVE.store(true, Ordering::SeqCst);
    for _ in 0..32 {
        for (target, strategy) in [(Protocol::OpenAiChat, Strategy::Auto),
            (Protocol::OpenAiResponses, Strategy::Direct), (Protocol::OpenAiResponses, Strategy::Ir)] {
            let result = convert(Protocol::OpenAiChat.into(), target.into(), Operation::Request,
                input, &mut output, Options { strategy, ..Options::default() },
                Workspace { tokens: &mut tokens, records: &mut records,
                            scratch: &mut scratch, nested_tokens: &mut nested });
            ok &= result.is_ok();
        }
    }
    ACTIVE.store(false, Ordering::SeqCst);
    assert!(ok);
    assert_eq!(ALLOCATIONS.load(Ordering::Relaxed), 0);
    assert_eq!(BYTES.load(Ordering::Relaxed), 0);
}
