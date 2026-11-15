use zconv::json::Token;
use zconv::stream::*;
use zconv::{Error, Path, Protocol, Strategy};

fn config(source: Protocol, target: Protocol, strategy: Strategy) -> StreamConfig<'static> {
    let mut config = StreamConfig::new(source, target);
    config.strategy = strategy;
    config.context.created = Some(7);
    config.context.response_id = Some("r1");
    config.context.model = Some("m");
    config.context.item_id_prefix = Some("local_");
    config.context.initial_usage.input = Some(5);
    config
}

type RunResult = Result<(Vec<u8>, Path, Report, Usage, Memory), (Error, Vec<u8>)>;

fn run(input: &[u8], config: StreamConfig<'_>, split: usize, output_size: usize) -> RunResult {
    let mut framing = vec![0; 65536];
    let mut data = vec![0; 65536];
    let mut pending = vec![0; 65536];
    let mut tokens = vec![Token::EMPTY; 4096];
    let mut items = vec![ActiveItem::EMPTY; 64];
    let mut checkpoints = vec![ActiveItem::EMPTY; 64];
    let mut events = vec![Event::EMPTY; 256];
    let mut aggregate = vec![0; 65536];
    let mut converter = StreamConverter::new(
        config,
        StreamBuffers {
            framing: &mut framing,
            data: &mut data,
            pending: &mut pending,
            tokens: &mut tokens,
            items: &mut items,
            checkpoints: &mut checkpoints,
            events: &mut events,
            aggregate: &mut aggregate,
        },
    )
    .map_err(|error| (error, Vec::new()))?;
    let mut result = Vec::new();
    let mut position = 0;
    let mut output = vec![0; output_size];
    let mut turns = 0;
    loop {
        turns += 1;
        assert!(turns < input.len() * 500 + 10000, "no-progress loop");
        let end = (position + split).min(input.len());
        let progress = converter.step(&input[position..end], &mut output, end == input.len());
        position += progress.consumed;
        result.extend_from_slice(&output[..progress.written]);
        match progress.status {
            Status::Done if position == input.len() => {
                return Ok((
                    result,
                    converter.path(),
                    converter.report(),
                    converter.usage(),
                    converter.memory(),
                ));
            }
            Status::Failed(error) => return Err((error, result)),
            Status::NeedBuffer { buffer, needed, .. } => {
                panic!("insufficient {buffer:?}: {needed}")
            }
            _ => {}
        }
    }
}


use zconv::{convert, Operation, Options, Workspace, Format, Profile};
use zconv::ir::Record;
fn request(label: &str, input:&[u8], source:Format, target:Format, strategy:Strategy) {
    let mut tokens=[Token::EMPTY;512];
    let mut records=[Record::EMPTY;128];
    let mut nested=[Token::EMPTY;512];
    let mut scratch=[0;4096];
    let mut output=[0;8192];
    let mut options=Options::default();
    options.strategy=strategy;
    options.context.max_tokens=Some(32);
    let result=convert(source,target,Operation::Request,input,&mut output,options,Workspace{tokens:&mut tokens,records:&mut records,nested_tokens:&mut nested,scratch:&mut scratch});
    match result {
      Ok(result)=>println!("{label}: OK {:?} {}",result.path,String::from_utf8_lossy(&output[..result.written()])),
      Err(e)=>println!("{label}: ERR {e:?}"),
    }
}
fn main() {
 let wire=concat!(
 "data: {\"id\":\"r1\",\"model\":\"m\",\"created\":7,\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"x\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":1,\"total_tokens\":6}}\n\n",
 "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"y\"},\"finish_reason\":null}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":2,\"total_tokens\":7}}\n\n",
 "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":5,\"completion_tokens\":7,\"total_tokens\":12}}\n\n",
 "data: [DONE]\n\n").as_bytes();
 for strategy in [Strategy::Direct,Strategy::Ir] {
   let result=run(wire,config(Protocol::OpenAiChat,Protocol::OpenAiChat,strategy),1,1).unwrap();
   let out=String::from_utf8(result.0).unwrap();
   println!("CUMULATIVE {strategy:?}: final_output={:?} total={:?}, output_usage_objects={}\n{}", result.3.output,result.3.total,out.matches("\"usage\":").count(),out);
   assert_eq!(result.3.output,Some(7));
   assert_eq!(result.3.total,Some(12));
   assert_eq!(out.matches("\"usage\":").count(),1);
   assert!(!out.contains("\"completion_tokens\":1,"));
   assert!(!out.contains("\"completion_tokens\":2,"));
 }
 let chat=Format::new(Protocol::OpenAiChat);
 let responses=Format::new(Protocol::OpenAiResponses);
 let gemini=Format::new(Protocol::GeminiGenerateContent);
 let anthropic=Format::new(Protocol::AnthropicMessages);
 let mut vendor=chat; vendor.profile=Profile::OpenAiCompatible("deepseek");
 let stream_options=br#"{"model":"m","messages":[{"role":"user","content":"hi"}],"stream":true,"stream_options":{"include_usage":true,"continuous_usage_stats":true}}"#;
 request("STREAM_OPTIONS_IDENTITY",stream_options,chat,chat,Strategy::Auto);
 request("STREAM_OPTIONS_IR_SAME",stream_options,chat,chat,Strategy::Ir);
 request("STREAM_OPTIONS_RESPONSES",stream_options,chat,responses,Strategy::Auto);
 let objargs=br#"{"model":"m","messages":[{"role":"assistant","content":null,"tool_calls":[{"id":"c","type":"function","function":{"name":"f","arguments":{"x":1}}}]}]}"#;
 request("OBJECT_TOOL_ARGUMENTS_SAME",objargs,chat,chat,Strategy::Auto);
 request("OBJECT_TOOL_ARGUMENTS_RESPONSES",objargs,chat,responses,Strategy::Auto);
 let reason=br#"{"model":"m","messages":[{"role":"assistant","content":"answer","reasoning_content":"thought"}]}"#;
 request("REASONING_STANDARD_SAME",reason,chat,chat,Strategy::Auto);
 request("REASONING_STANDARD_ANTHROPIC",reason,chat,anthropic,Strategy::Auto);
 request("REASONING_VENDOR_ANTHROPIC",reason,vendor,anthropic,Strategy::Auto);
 request("REASONING_VENDOR_RESPONSES",reason,vendor,responses,Strategy::Auto);
 request("REASONING_VENDOR_GEMINI",reason,vendor,gemini,Strategy::Auto);
 let alias=br#"{"model":"m","messages":[{"role":"assistant","content":"answer","reasoning":"thought"}]}"#;
 request("REASONING_ALIAS_VENDOR_ANTHROPIC",alias,vendor,anthropic,Strategy::Auto);
 let video=br#"{"model":"m","messages":[{"role":"user","content":[{"type":"video_url","video_url":{"url":"https://example.org/video.mp4"}}]}]}"#;
 request("VIDEO_URL_GEMINI",video,chat,gemini,Strategy::Auto);
 let audio=br#"{"model":"m","messages":[{"role":"user","content":[{"type":"audio_url","audio_url":{"url":"https://example.org/audio.wav"}}]}]}"#;
 request("AUDIO_URL_GEMINI",audio,chat,gemini,Strategy::Auto);
}
