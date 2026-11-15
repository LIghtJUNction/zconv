# zconv API 能力核验报告

日期：2026-11-15（Asia/Taipei）

## 1. 结论与核验基线

本报告按 Project Z 仓库 [LIghtJUNction/zconv](https://github.com/LIghtJUNction/zconv) 核验用户提到的“zcanv”。

- 源码提交：`5c1e0eb8a3b3119ce5dd1af7a377605372241d50`。
- Cargo 版本：`0.3.0`。
- 对照清单：[dynamo-protocols 10.0.2 README](https://docs.rs/crate/dynamo-protocols/10.0.2)。
- 对照源码固定在其发布提交：`e49538ea5bd5203a09971b5a1517772c19fbdef8`。
- 方法：源码与现有测试审阅，执行现有测试，再用独立程序检查具体 JSON 和 SSE 输入。

**zconv 已实现主要对话协议的核心转换能力，但没有覆盖这份清单的全部 API 与扩展语义。** Batch、Files 管理、Embeddings、Images 生成/编辑缺少独立协议适配；Chat 持续 usage 输出、部分供应商扩展，以及 Responses 宽松输入兼容仍有明确缺口。

“owned”表示类型由当前 crate 定义维护；“re-exported”表示通过 `pub use` 导出依赖库的类型；“input chain”表示请求及其嵌套输入类型。这些词描述类型组织方式。zconv 没有 upstream SDK 依赖，其请求、响应和流转换均由自身实现。服务器会话存储、HTTP 调用和任务执行由调用方负责。

## 2. 逐项支持矩阵

| 用户要求 | zconv 0.3.0 的实际情况 | 判定 |
|---|---|---|
| Chat Completions | 请求、完整响应、SSE 均有自有实现 | 核心支持 |
| Chat multimodal | 图片 URL/base64、wav/mp3 input_audio、部分文件内容；video_url/audio_url 缺少语义转换 | 部分支持 |
| reasoning content：DeepSeek-R1 / QwQ | 显式兼容 profile 识别相关字段；完整消息与流的字段别名、转换方向有差异 | 有条件支持 |
| continuous usage stats | 能读取并累计更新输入 usage；输出 Chat 流只在结束时发送 usage | 持续输出未实现 |
| tool-calling | tools/tool_calls、旧 functions/function_call、工具结果、流式参数片段有实现；object arguments 被拒绝 | 标准形式支持，扩展有缺口 |
| Batch API | 没有独立请求/响应类型或适配器 | 未实现 |
| Files API | 没有上传、查询、下载、删除等资源 API 类型或适配器 | 未实现 |
| Responses API | 自有请求、响应、流实现；部分宽松输入可用，部分拒绝或在 IR 重编码失败 | 核心支持，宽松兼容不足 |
| Completions | 自有请求、响应和 SSE；单条文本 prompt 可转换，batch/token prompt 需外部处理 | 核心支持 |
| Anthropic Messages | 自有消息、工具、图片、thinking、usage 及流生命周期实现；扩展有范围限制 | 核心支持 |
| Embeddings | 没有独立类型或适配器 | 未实现 |
| Images | 没有图像生成/编辑 API 类型或适配器 | 未实现 |
| 上述各项的 upstream re-export | dependencies、dev-dependencies、build-dependencies 均为空 | 不采用此方式 |

存在图片或 file_id 消息块，只能说明消息内容有对应表示。Images API 还涉及生成/编辑的请求与响应，Files API 还涉及文件资源操作。把批量 prompt 拆开处理也不能替代 Batch API 的任务描述与结果封装。

直接依据：[Cargo.toml](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/Cargo.toml)、[Protocol 枚举](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/types.rs)、[公开模块](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/lib.rs)。

## 3. Chat：支持细节和实测差异

### 3.1 多模态

已实现的内容包括 `image_url`、data URL、`input_audio` 的 wav/mp3、`file`/`input_file` 的部分 URL、内联数据和文件引用。data URL 可拆解成目标协议的 MIME 与 base64 字段。

Dynamo 接受的 `video_url`、`audio_url` 在 zconv 中会成为 opaque 内容块。同格式可保存原始数据；实测 Chat → Gemini 均返回：

```text
Unsupported { feature: "opaque content block" }
```

提供商 file_id 带有来源命名空间。跨提供商转换需要调用方先完成文件映射，否则返回 `ContextRequired { field: "mapped provider file reference" }`。

源码：[content_block 与媒体解析](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/decode.rs)、[媒体与 opaque 内容编码规则](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/encode.rs)。

### 3.2 reasoning_content

兼容模式由调用者明确选择：

```rust
let mut source = zconv::Format::new(zconv::Protocol::OpenAiChat);
source.profile = zconv::Profile::OpenAiCompatible("deepseek");
```

这里的字符串用于标识提供商；代码没有单独的 DeepSeek-R1/QwQ 模型实现，也不负责从原始模型文本中提取思考过程。

| 输入/路径 | 核验结果 |
|---|---|
| Standard Chat → 同格式 Identity，含 reasoning_content | 原样保留，属于未归一化扩展 |
| Standard Chat → Anthropic，含 reasoning_content | 拒绝 source-owned extension |
| Compatible Chat → Anthropic，含 reasoning_content | 成功转换为 thinking |
| Compatible Chat → Responses/Gemini，完整消息含 reasoning_content | 拒绝 reasoning summary and provider thought differ |
| Compatible Chat → Anthropic，使用 reasoning 别名 | 拒绝 source-owned extension |
| Compatible Chat SSE，使用 reasoning_content 或 reasoning | 流解析器识别两种字段；现有测试覆盖 Chat ↔ Gemini |
| Standard Chat SSE，含 reasoning 字段 | 拒绝未声明的推理扩展 |

完整消息中的 reasoning 字段目前要求字符串；Dynamo 的分段 reasoning 数组扩展没有相同的归一化路径。Dynamo 的 model/profile 兼容性与模型服务本身的运行支持应分别核验。

源码：[chat_message](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/decode.rs)、[reasoning preflight](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/encode.rs)、[流式 reasoning 处理](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/stream/decode.rs)。

### 3.3 continuous_usage_stats

请求中的下列字段会作为源格式扩展保留：

```json
{"stream_options":{"include_usage":true,"continuous_usage_stats":true}}
```

它没有进入标准化请求参数或输出流发送策略。同格式 Identity/IR 可保留；Chat → Responses 默认严格策略返回 `PolicyViolation { feature: "source-owned extension" }`。

对输入 Chat SSE 提供三次累计 completion_tokens：`1 → 2 → 7`，prompt_tokens 始终为 5。Direct 与 IR 两条重编码路径均得到：

| 检查项 | 结果 |
|---|---|
| 最终 output tokens | 7 |
| 最终 total tokens | 12 |
| 输出 SSE 中 usage 对象数量 | 1 |
| 中间的 1 和 2 是否随输出 chunk 发送 | 否 |
| usage 发送时机 | 结束前的独立 usage chunk |

累计计数没有重复相加。`StreamConverter::usage()` 可供宿主读取当前统计；编码器的 `usage()` 只更新状态，最终 `finish()` 才写出 Chat usage。因此当前实现适合获取累计统计，尚缺逐 chunk 输出策略。

证据：[stream.rs](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/stream.rs)、[stream/encode.rs](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/stream/encode.rs)、[完整探针输出](2026-11-15/chat-results.txt)。

### 3.4 tool-calling

已有工具定义、选择、调用、结果、旧函数协议与交错的流式参数片段。Dynamo 接受字符串或对象形式的 FunctionCall.arguments；zconv 的 Chat 路径要求字符串。

以下 arguments 可以进入标准路径：

```json
{"arguments":"{\"x\":1}"}
```

以下 Dynamo 扩展形式在 zconv 的 Identity 与 Chat → Responses 中都报错：

```json
{"arguments":{"x":1}}
```

错误为 arguments expected string。Responses 中带 text/image/file 数组的工具结果可在同源保存，但跨协议转换返回 `Unsupported { feature: "typed tool-result content array across protocols" }`。

证据：[zconv chat_call / response_item](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/decode.rs)、[Dynamo arguments 解析器](https://github.com/ai-dynamo/frontend-crates/blob/e49538ea5bd5203a09971b5a1517772c19fbdef8/protocols/src/types/chat.rs)。

## 4. Responses：宽松输入实测

每个样例分别执行：

1. 同格式 Auto，实际选择 Identity。
2. 同格式强制 IR 解码/重编码。
3. Responses → Chat Auto。

默认使用严格保真策略。表中的失败是实际观察到的当前行为；部分拒绝体现跨协议语义边界，部分暴露输入兼容与路径一致性的不足。

| 输入形态 | 同格式 Identity | 同格式 IR | 转 Chat |
|---|---|---|---|
| assistant 缺少 id/status，output_text 缺少 annotations | 成功 | 成功 | 成功，Direct |
| assistant 缺少 content | required field missing | required field missing | missing field |
| assistant content:null | 保留 | missing or null content in target item | 成功，保留 null |
| output_text annotations:null | 保留 | 保留 | source-owned extension |
| output_text annotations:[] | 保留 | 保留 | 成功，Direct |
| reasoning 缺少 id/summary，只含 encrypted_content | 保留 | 成功，写出 summary:[] | reasoning summary and provider thought differ |
| reasoning summary:null | expected array | expected array | expected array |
| input_image 缺少 detail | 保留 | 保留缺失 | 成功，目标 detail 缺失 |
| input_image detail:null | 保留 | 保留 null | 成功，目标 detail:null |
| function_call → assistant → function_call_output，普通文本 | 成功 | 成功 | 成功，Direct |
| function_call_output 包含图片数组 | 保留 | 保留 | typed tool-result content array across protocols |
| text:{"verbosity":"low"} | Responses text configuration extension | 同左 | 同左 |
| previous_response_id | 保留 | 保留 | 要求 resolved source conversation history |

Dynamo 10.0.2 的固定发布源码明确把 assistant content 缺失/null、annotations 缺失/null、reasoning summary 缺失/null 以及 image detail 缺失/null 归一化为对应默认值。zconv 对部分缺失字段兼容，但没有完全采用这些规则。detail:null 在 zconv 中仍是 null，不能把“解析成功”写成“已归一化为 auto”。

`previous_response_id` 是源服务中的历史引用。zconv 不查询远程会话；跨协议前应由宿主取出历史并并入请求。通过 drop_context 移除引用会丢失这项上下文，需要显式业务决策。

证据：[Dynamo 固定发布源码](https://github.com/ai-dynamo/frontend-crates/blob/e49538ea5bd5203a09971b5a1517772c19fbdef8/protocols/src/types/responses/mod.rs)、[zconv 解码](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/decode.rs)、[zconv 编码](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/encode.rs)、[39 次转换的完整输出](2026-11-15/responses-results.txt)。

## 5. Completions 与 Anthropic 的范围

### Legacy Completions

请求、完整响应和 SSE 都由 zconv 自己解析与编码。单个文本 prompt 可转换为一条 user 消息；token ID prompt、字符串批次和 token 批次可以通过同格式验证，但 IR 转换要求调用方先进行 tokenizer 解码或批次拆分。

反向写成 Completions 时，当前实现要求一条 user 消息和一个文本块。任意多轮聊天、工具历史、多媒体内容无法直接展平。

### Anthropic Messages

支持 system、user/assistant 消息、text、tool_use/tool_result、thinking/redacted_thinking 的完整消息、图片 URL/base64、usage/cache counters，以及常规流生命周期。

限制包括：

- 顶层 thinking 配置仍作为源格式扩展保存，未统一成跨协议参数。
- server tools 等未知内容块缺少跨协议语义表示。
- 流式支持的块种类比完整消息少；redacted_thinking 等流式起始块会被拒绝。
- 非空流式 citations、部分 server-tool 细分 usage 会被拒绝。
- 提供商签名不能无条件跨提供商复用。

证据：[协议解码器](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/protocol/decode.rs)、[流解码器](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/src/stream/decode.rs)、[现有协议测试](https://github.com/LIghtJUNction/zconv/blob/5c1e0eb8a3b3119ce5dd1af7a377605372241d50/tests/protocol.rs)。

## 6. 补齐工作的建议顺序

这是依据当前差异提出的工程建议。

| 优先级 | 工作 | 验收依据 |
|---|---|---|
| P0 | Responses 缺失/null 字段、text.verbosity、同格式 IR 路径一致性 | 表中兼容样例分别验证 Identity、IR、跨协议结果 |
| P0 | 声明并实现 continuous usage 输出策略 | 1→2→7 输入在持续模式有中间输出，最终统计仍为 7 |
| P0 | object arguments 归一化，完整消息 reasoning 别名与分段内容 | 与固定对照版本样例一致，保持顺序与语义 |
| P1 | Embeddings 与 Images 独立请求/响应表示 | 覆盖其真实端点格式，和消息媒体内容分别验收 |
| P1 | Batch / Files 资源封装、文件与批处理引用表示 | 明确 wire 类型与宿主资源操作之间的职责 |
| P2 | video_url/audio_url、工具结果媒体数组、更多 Anthropic 扩展 | 为每个转换方向声明支持/拒绝/损失规则 |

保持 Project Z 的 no_std、零依赖目标时，可以自行定义所需借用视图或 IR。若引入 upstream re-export，会改变当前依赖边界。网络、鉴权、上传下载和任务调度可放在宿主或独立接入层；zconv 专注字节格式与语义转换。

## 7. 验证结果与复现

现有测试执行：

```sh
cargo test --locked --offline --lib --tests
```

结果：**133 passed，0 failed**。另执行 39 次 Responses 路径检查、13 次 Chat 请求检查及 Direct/IR 两次持续 usage 流检查。定向探针用于记录支持与失败行为，并非把所有输入都视为应成功的测试。

复现源文件：

- [Chat 与 SSE 探针](2026-11-15/chat-probe.rs)
- [Chat 与 SSE 原始输出](2026-11-15/chat-results.txt)
- [Responses 探针](2026-11-15/responses-probe.rs)
- [Responses 原始输出](2026-11-15/responses-results.txt)

在包含本报告的仓库根目录，使用 Rust 1.100.0 可运行：

```sh
cargo test --locked --offline --lib --tests
rustc --edition=2024 --crate-name zconv --crate-type rlib src/lib.rs -o /tmp/libzconv-api-audit.rlib
rustc --edition=2024 docs/audits/2026-11-15/chat-probe.rs --extern zconv=/tmp/libzconv-api-audit.rlib -o /tmp/zconv-chat-api-audit
/tmp/zconv-chat-api-audit
rustc --edition=2024 docs/audits/2026-11-15/responses-probe.rs --extern zconv=/tmp/libzconv-api-audit.rlib -o /tmp/zconv-responses-api-audit
/tmp/zconv-responses-api-audit
```

这是本地协议格式、IR 和 SSE 核验，未调用真实模型服务，也未执行 Dynamo 的服务端实现。Dynamo 输入规则依据固定发布源码读取。对某个现成 SDK 的端到端兼容还应使用其实际请求样例验证。

## 8. 结论自查

| 维度 | 检查结果 |
|---|---|
| 对任务的作用 | 覆盖用户列出的全部 API 家族，并列出具体缺口 |
| 表述清晰度 | 区分类型归属、内容块、独立 API、同源保留与语义转换 |
| 实验证据 | 既有 133 项测试与独立探针输出均支持相应结论 |
| 评估范围 | 写明固定版本、本地输入范围与未执行的服务端验证 |
| 设计边界 | 补齐建议保留 no_std/零依赖约束，外部资源操作另有承担方 |

本次提交包含分析报告与复现材料。
