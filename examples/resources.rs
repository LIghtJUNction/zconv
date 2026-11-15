//! Resource wire messages are independent of the six conversation adapters.
use zconv::api::{self, EmbeddingInput, EmbeddingsRequest, Encoding, FileUpload, Schema};
use zconv::json::{Limits, Token};

fn main() -> Result<(), zconv::Error> {
    let mut request =
        EmbeddingsRequest::new("text-embedding-3-small", EmbeddingInput::Text("Hello"));
    request.dimensions = Some(256);
    request.encoding_format = Some(Encoding::Float);
    let mut request_bytes = [0; 512];
    let n = request.encode(&mut request_bytes)?;
    let mut tokens = [Token::EMPTY; 64];
    let body = api::parse(
        Schema::EmbeddingsRequest,
        &request_bytes[..n],
        &mut tokens,
        Limits::default(),
    )?;

    let mut jsonl = [0; 1024];
    let n = api::encode_batch_input("embedding-1", "/v1/embeddings", body.value(), &mut jsonl)?;
    jsonl[n] = b'\n';
    let upload = FileUpload {
        filename: "embeddings.jsonl",
        content_type: "application/jsonl",
        contents: &jsonl[..n + 1],
        purpose: "batch",
        expires_after: Some(86_400),
    };
    let mut multipart = [0; 2048];
    let written = upload.encode("zconv-example-boundary", &mut multipart)?;
    println!(
        "Batch JSONL: {}",
        core::str::from_utf8(&jsonl[..n]).unwrap()
    );
    println!("Multipart body: {written} bytes; send using your own HTTP transport");
    // Content-Type: multipart/form-data; boundary="zconv-example-boundary"
    Ok(())
}
