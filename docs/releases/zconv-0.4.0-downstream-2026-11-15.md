# zconv 0.4.0 与下游迁移发布记录

核对时间：2026-11-15 09:52 UTC。此记录覆盖本次发现并可操作的 6 个仓库：zconv、zcall、zharness、zmcpio、lmm-api-source 与 lmm-api。公开依赖关系和本次已检查仓库构成范围依据，不能据此证明不存在其他第三方或私有消费者。

## 1. 发布状态

zconv 0.4.0、zcall 0.3.0、zharness 0.5.0 与 zmcpio 0.4.0 已完成 GitHub Release 和 crates.io 发布。四个 Rust 库的发布与依赖升级已完成。LMM 的协议迁移和发布器已推送，但源码发布工作流尚无运行记录；父配方发布依赖源码发布完成。

| 仓库 / 发布单元 | 本轮版本 | 可证明状态 | 主要证据 |
| --- | --- | --- | --- |
| zconv | 0.4.0 | 正式 GitHub Release 与 crates.io 包已发布 | [Release](https://github.com/LIghtJUNction/zconv/releases/tag/v0.4.0)、[crates.io](https://crates.io/crates/zconv/0.4.0)、[发布流程](https://github.com/LIghtJUNction/zconv/actions/runs/43696550506) |
| zcall | 0.3.0 | 正式 GitHub Release 与 crates.io 包已发布；后续 macOS 测试夹具修复的主线 CI 全部通过 | [Release](https://github.com/LIghtJUNction/zcall/releases/tag/v0.3.0)、[crates.io](https://crates.io/crates/zcall/0.3.0)、[当前主线 CI](https://github.com/LIghtJUNction/zcall/actions/runs/43699181060) |
| zmcpio | 0.4.0 | 正式 GitHub Release 与 crates.io 包已发布；CI、传输回归、性能证据和发布流程均通过 | [Release](https://github.com/LIghtJUNction/zmcpio/releases/tag/v0.4.0)、[crates.io](https://crates.io/crates/zmcpio/0.4.0)、[发布流程](https://github.com/LIghtJUNction/zmcpio/actions/runs/43699182713) |
| zharness | 0.5.0 | 正式 GitHub Release 与 crates.io 包已发布；CI、性能证据和发布流程均通过 | [Release](https://github.com/LIghtJUNction/zharness/releases/tag/v0.5.0)、[crates.io](https://crates.io/crates/zharness/0.5.0)、[发布流程](https://github.com/LIghtJUNction/zharness/actions/runs/43700355281) |
| lmm-api-source | 顶层 VERSION 0.1.20 | 协议迁移、registry 锁文件、发布器和请求已推送；未创建 v0.1.20 Release | [源码候选](https://github.com/TokenNotIncluded/lmm-api-source/commit/28eedde22963b1be92964bd2bf2325d65664d339)、[Actions](https://github.com/TokenNotIncluded/lmm-api-source/actions) |
| lmm-api 配方 | API 0.1.23 / Core 0.1.18 | 发布器已推送；两项版本更新已在工作区准备，等待已发布 source SHA 后更新 gitlink 与来源记录 | [发布器提交](https://github.com/TokenNotIncluded/lmm-api/commit/ac27f20914a38daa8720c17834deeed933178001) |

LMM 的两个计划发布都是带来源、检查日志和 SHA-256 校验清单的源码或配方预发布。源码中的 Rust `lmm-core` 包仍为 `0.1.0`、`publish = false`；它与顶层源码版本、Core 配方版本各自独立。

## 2. 实现范围与最初清单的对应

zconv 的职责是校验、读取和重写协议字节。它把输入中的字段映射成调用方提供存储空间的结构，再写入调用方的输出缓冲区。网络连接、模型执行和服务端任务由上层应用负责。因此，本次保留 `no_std`、safe Rust、零依赖和核心零分配约束，资源 API 使用本库自有契约；没有引入或重新导出 upstream SDK 类型。

| 用户关注的能力 | 0.4.0 实现 | 明确边界 |
| --- | --- | --- |
| Chat Completions 多模态 | 保留可表示的图片、音频 URL、视频 URL 和嵌套多模态工具结果；接受对象形式的工具参数 | 目标协议不能表达的原生媒体继续返回明确错误；不代表任意 Chat 媒体输出 delta 均已实现 |
| DeepSeek-R1 / QwQ 风格 reasoning | compatible profile 下，非流式请求/完整响应接受 `reasoning_content` / `reasoning` 的字符串或有序字符串数组；SSE 支持字符串 reasoning delta；完整响应和流式链路区分推理原文与摘要 | SSE reasoning 数组不在该兼容范围内；不把摘要改名成原始思考；签名与提供方来源限制仍生效；这不是对指定在线模型服务的调用验证 |
| Continuous usage stats | 增加类型化 `StreamOptions` 与可选连续输出策略；默认仍只输出最终用量；累计快照覆盖旧快照 | `1 → 2 → 7` 的最终值是 7，不会累计为 10；缺失计数保持未知；Responses/Interactions 的中途用量输出继续明确拒绝 |
| Tool calling | 兼容对象参数、内容数组工具结果、嵌套内容跨度以及相关历史/流式事件 | Anthropic 服务端工具不产生本地 `ToolReady`；不可表达的跨提供方内容明确拒绝 |
| Batch API | 创建、查询、列表、取消端点元数据，输入与结果 JSONL，批次状态/计数/错误/文件关联 | 调用方上传文件、检查整文件 custom ID 唯一性、轮询及下载结果；内层请求需由对应协议适配器验证 |
| Files API | 二进制 multipart 上传、用途/过期时间、文件元数据、列表/删除/下载路径 | 文件下载响应是原始字节；HTTP 与 MIME 处理归调用方 |
| Responses API | 接受 Codex/Agents 风格缺省或 null content、summary、annotations、image detail；支持 text.verbosity；保留 raw reasoning 与 summary 的独立索引/生命周期 | 输入兼容放宽没有补造答案或参数；输出链也是本库实现，并非 upstream 类型重新导出 |
| Completions | 保留自有请求/响应转换，接入连续用量和相关流式兼容 | tokenization、批量 prompt 展开归调用方；响应类型没有变成 upstream SDK 依赖 |
| Anthropic Messages | 保留自有协议实现；增加 provider block/delta/usage 事件，覆盖 redacted thinking、citations、服务端工具及详细用量的同源保留 | 不透明内容和签名的跨提供方转换受来源约束；zconv 能保留的事件仍需下游主动接入 |
| Embeddings | 类型化单文本/文本数组/token IDs/token batches 输入，float/base64 响应、索引、维度参数及用量 | 不做分词、向量计算、base64 解码或模型维度保证 |
| Images | 生成 JSON、编辑 JSON/multipart、图片响应以及 partial/completed SSE 契约 | 不生成或解码图片；legacy variation 的格式兼容不表示对应服务仍可用 |

资源模块包含 **15 种 schema 和 13 种 endpoint 变体**。已知字段被校验，未知字段保持借用并可原样编码；保留字段并不等于理解其语义。端点 ID 按单个路径段编码，JSONL 压缩不改动字符串内容，multipart 文件部分保持二进制字节。编码输出缓冲区不足时报告精确所需字节数；解析 token 工作区不足时的数量仍可能只是下界。

`StreamOptions` 表达请求偏好，流式编码器的连续用量策略仍由应用显式选择和连接。请求里出现该字段不保证远端服务实际发送用量；转换器只处理观察到的计数。

对应实现和边界可在发布提交的 [CHANGELOG](https://github.com/LIghtJUNction/zconv/blob/1488f0abab5c4f926f6ac258f62108a5cb7f4ff9/CHANGELOG.md)、[资源契约](https://github.com/LIghtJUNction/zconv/blob/1488f0abab5c4f926f6ac258f62108a5cb7f4ff9/docs/RESOURCE_APIS.md)、[协议支持矩阵](https://github.com/LIghtJUNction/zconv/blob/1488f0abab5c4f926f6ac258f62108a5cb7f4ff9/docs/SUPPORT.md) 与 [流式说明](https://github.com/LIghtJUNction/zconv/blob/1488f0abab5c4f926f6ac258f62108a5cb7f4ff9/docs/STREAMING.md) 中核对。

## 3. 下游依赖与行为更新

| 消费方 | 精确依赖 / 固定来源 | 本次行为更新 |
| --- | --- | --- |
| zcall 0.3.0 | `zconv = "=0.4.0"`，关闭默认特性 | 重新导出资源入口及其可用 JSON 类型；接入 verbosity、stream options、continuous usage、raw/summary 区分和 provider 事件边界；修复嵌套工具结果替换后的孤立子块 |
| zharness 0.5.0 | `zcall = "=0.3.0"`，锁定 registry zconv 0.4.0 | 更新事件枚举处理与执行钩子，验证真实 native/Wasm/mixed 路径；修正已预升版本候选的发布规划检查 |
| zmcpio 0.4.0 | `zcall = "=0.3.0"`，锁定 registry zconv 0.4.0 | 升级原先较旧的整条依赖链；验证 stdio、client、HTTP 和 Wasm 编译边界 |
| LMM Core 候选 | `zconv = "=0.4.0"`，正式 registry lock | 接入 `ReasoningSummary`、raw/summary 独立索引与生命周期、请求历史及 Gemini thought/signature 映射；保持无法表示事件的 Unsupported 行为 |
| LMM 父配方 | `generated/lmm-api-source` gitlink | 需在 source 实际发布后固定其 tag 解引用 SHA，并更新 API/Core 的 SOURCE.md 与 release request |

这些 Rust manifest 和 lock 没有遗留本次迁移使用的 `[patch.crates-io]` 或本地 zconv/zcall 路径覆盖。zcall 独立 fuzz 工程对本仓库的 `path = ".."` 保持正常用途。父配方 gitlink 当前仍为 `4a64b28227f8908b5613e3aabb5a7374b10afc48`，未将尚未发布的源码候选冒充成发布依赖。

公开 IR 的新增字段和变体需要 exhaustive match / struct literal 消费方同步修改，因此使用新的 minor 版本。zconv、zcall、zharness、zmcpio 的最低 Rust 版本为 1.99.1；LMM Core 自身声明 Rust 1.100.0。

LMM 的消费范围比 zconv 的格式保留能力窄。Core 不能表示的 provider-owned server-tool、opaque block、citation 和详细 usage 会继续返回 Unsupported，不会产生本地工具执行或假成功终态。此次升级也没有自动给 LMM 网关增加 Batch、Files、Embeddings、Images HTTP 路由。

## 4. 验证结果

| 对象 | 已观察到的验证 | 结果与实际含义 |
| --- | --- | --- |
| zconv | Rust 1.100.0 与 1.99.1 各 187 项集成测试；release 187 项 + 1 项文档测试；23 项资源、19 项非流式兼容、11 项流式兼容测试 | 全部通过；资源/兼容数量是总测试中的子集 |
| zconv | 两项带正对照的分配检测、三个交叉编译目标、fmt、严格 Clippy、包审计/提取包、14 项发布脚本测试、4 项独立 JSON oracle | 全部通过；受测核心和资源路径观测到零分配；交叉编译不等于设备运行 |
| zcall | MSRV debug 与 stable release 各 127 项测试、2 项文档测试、29 项 Python 测试、包校验与发布演练 | 全部通过；新增回归覆盖连续用量、嵌套内容编辑、跨度迁移和服务端工具隔离 |
| zcall 当前主线 | [CI 43699181060](https://github.com/LIghtJUNction/zcall/actions/runs/43699181060) 的 12/12 jobs，包含 Miri 和六组 OS/toolchain 测试 | 全部成功；原 macOS 1.99.1 HTTP 夹具在 debug/release 均 2 passed |
| zmcpio | 独立 MSRV all-features/all-targets 209 passed、0 failed、2 ignored；strict Clippy、debug/release、真实 stdio/client/HTTP smoke、Wasm、发布包检查 | 本地与远端发布门禁通过；两项 ignored 是由父测试实际执行的子进程夹具 |
| zharness | stable 完整 scripts/check.py；独立 MSRV all-targets/all-features 116 passed、0 failed；Wasm/Thumb/Thumb+Wasm-host；native/Wasm/mixed；24 例 benchmark smoke；30 项 Python 门禁 | 全部通过；干净归档来源、all-features 提取包、publish dry-run、两次打包字节一致也已通过 |
| zharness 远端 | [CI](https://github.com/LIghtJUNction/zharness/actions/runs/43700355257)、[性能证据](https://github.com/LIghtJUNction/zharness/actions/runs/43700355224)、[Release](https://github.com/LIghtJUNction/zharness/actions/runs/43700355281) | 全部 success；Release 的 plan、六组 OS/toolchain 测试、portability 和 publish 共 9 个 jobs 成功 |
| LMM source 候选 | production `cargo clippy --locked --lib -- -D warnings`；16 项 workflow topology、4 项 source release guard、docs checker、actionlint | 已通过；Core 新增 8 项回归尚无成功执行证据 |
| LMM source 本地 libtest | 两次 libtest 代码生成被 SIGKILL；后续 metadata 类型检查主动停止 | 没有把编译尝试计为测试通过；保留失败日志，等待真正的 source qualify job |
| LMM 父发布器 | 4 项 guard tests；两个配方目录及归档的 Recipkg 检查；recursive prompt 生成；YAML/Python/Bash 与 actionlint | 发布器准备检查通过；不能代替最终源码 SHA 固定后的发布验证 |

zcall v0.3.0 的初次主线 CI 曾在 macOS 最低版本的 HTTP 测试夹具失败：监听器设为 nonblocking 后，接受的 socket 未显式切回 blocking。修复只调整测试夹具，保留原有断言与超时；当前主线重新验证成功。既有 v0.3.0 tag 与 registry 包保持不可变，后续修复没有改写已发布归档。

zharness 发布规划修复要求候选 SemVer 严格高于既有发布 tag，并保留同 tag 恢复时的来源检查。所有者、未发布版本、精确 main SHA、不可变 tag 和包来源检查保持生效。测试没有通过降低或跳过发布门禁来取得成功。

本轮没有进行真实付费模型调用、远端 Batch 作业或独立 LMM 验收，也没有部署生产服务。库的格式与状态机测试不能证明特定在线模型、私有扩展或远端服务可用。

## 5. 已发布包的来源与校验和

| 包 | 发布 tag 解引用 commit | crates.io SHA-256 |
| --- | --- | --- |
| zconv 0.4.0 | `1488f0abab5c4f926f6ac258f62108a5cb7f4ff9` | `fb4c2c557b09be10a449b44d4034059837ab1980e8f89b830aedefcc9d0bbc15` |
| zcall 0.3.0 | `9fbae3b620edec76e35803b2b8ecb2d8190136c0` | `82a8650e8302ae5e0ebd861e9062809c9d9162d0a839c7e8f9b6e54afbd2d20d` |
| zmcpio 0.4.0 | `bc89d7fa3b0249b3a9037748e2113b31615e8c89` | `1530747e76256c89865b521c407c520766fc93badae9c8b573e5a4e0c4e820ad` |
| zharness 0.5.0 | `e6b3a9746ab0d414121836129242d75e4b950890` | `76dcd53a4d6ff3f64e6788eadc5d31ce2875e284d3793de7679070833c86b1e9` |

zmcpio 实际下载归档为 151,699 bytes，其 `.cargo_vcs_info.json`、tag、Release run HEAD 三者一致；包内 lock 的 zcall/zconv checksum 与 registry 一致。zharness 实际下载包的 SHA-256 与 registry API、sparse index 三者一致，包内 VCS 与 tag / Release run 一致；包内 manifest 精确依赖 zcall 0.3.0，lock 锁定正式 zconv 0.4.0。可从 [zconv index](https://index.crates.io/zc/on/zconv)、[zcall index](https://index.crates.io/zc/al/zcall)、[zmcpio index](https://index.crates.io/zm/cp/zmcpio)、[zharness index](https://index.crates.io/zh/ar/zharness) 复核各版本记录。

## 6. LMM 的剩余发布条件

截至此记录，源仓库 main 的准确提交为 `28eedde22963b1be92964bd2bf2325d65664d339`，VERSION、registry lock 与 `.github/source-release-request.json` 已同步。GitHub Actions 查询没有返回该提交的运行记录。现有 GitHub 连接器不提供 Actions 设置或 workflow dispatch 操作；浏览器安全登录没有完成。因此，已确认的是“流程尚未开始”，尚无证据把原因确定为仓库禁用、组织策略或其他设置。

最终源码 tree 为 `a938ac129668055851ae3411ad12899d9eae9815`。该树再次通过候选 guard、4 项 guard 自测、16 项 topology 检查、docs-only 与 actionlint；静态检查日志 SHA-256 为 `fc2b28452b809f9d3069b21c0f43da8ea884031688f1fe3dabe2cc32c8591f8b`。这些静态检查没有执行 Rust 行为回归。

现有 [Qualified source release workflow](https://github.com/TokenNotIncluded/lmm-api-source/blob/28eedde22963b1be92964bd2bf2325d65664d339/.github/workflows/source-release.yml) 同时支持 main 发布请求和手动启动。它先检查精确来源、声明版本与正式 registry 依赖，再执行格式、严格 Clippy、relay 与受影响 edge/auth 单元测试；只有 qualify 全部通过才允许创建源码预发布。发布阶段再次检查 main 未移动、tag 来源一致和归档校验和，不覆盖既有 release。

恢复顺序如下：

1. 在源仓库 Actions 页面核实工作流可运行状态；若页面要求启用，由具备权限的维护者完成，然后在 main 启动 `Qualified source release`。不调整组织安全策略来跳过限制。
2. 等 source qualify 与 publish 成功，核对 v0.1.20 Release、tag 解引用 SHA、源码归档和校验清单。任何真实测试失败都需修复后重新资格检查。
3. 以实际发布的 source SHA 更新父仓库 `generated/lmm-api-source`、API/Core 的 `SOURCE.md`，提交 API 0.1.23 与 Core 0.1.18 配方版本。
4. 对最终配方检查后提交准确的 `.github/recipe-release-request.json`，运行 `Qualified recipe release` 并发布 `recipes-v0.1.23` 预发布。
5. 下载核对两份配方归档、父源码快照、source.json、检查日志和 SHA256SUMS，再将两个 LMM 单元标记为发布完成。

父发布器已固定所使用的 Recipkg CLI commit，并交叉校验公开 source release、source tag、gitlink 和 main。Contracts、Extensions、Web、CLI 配方的要求本轮未变，版本保持原值；旧 Go 来源记录与技能 gitlink 也保留。
