use kvim_core::{BufferBytesMax, TextBuffer};

fn main() {
    let buffer = TextBuffer::from_text("outside workspace\n", BufferBytesMax::default())
        .expect("the supplied text is bounded");
    assert_eq!(buffer.to_string(), "outside workspace\n");
}
