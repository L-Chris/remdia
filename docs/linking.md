# 内置组件与单库 SDK

demux/decoder/filter/encoder/mux/sink 仍是独立工厂，任意兼容插件图、端口、背压、预算、取消、generation 和帧所有权协议保持不变。默认发行方式把内置工厂统一链接，让链接器去重公共代码。动态插件 ABI 2.2 保留，用于额外扩展及 macOS 的独立 LAME 编码器。

## Rust 应用内置

```toml
[dependencies]
remdia = { version = "0.3.0", features = ["bundled"] }
```

当前开发版在发布前使用仓库路径或固定 Git revision。应用调用：

```rust
let engine = remdia::Engine::bundled(256 * 1024 * 1024)?;
// 或附加一个可信扩展目录；不存在的目录允许为空。
let engine = remdia::Engine::bundled_with_plugins("plugins", 256 * 1024 * 1024)?;
```

此模式无需另发系统音视频/图片/基础处理 DLL。内置节点直接传递 Rust MediaBuffer，不再为库间调用生成、解析 JSON 帧头，也不额外包一层 C ABI owner。Android 的 Java/JNI 边界仍有 JSON 和像素复制；本次没有将它改成零复制。

最小核心的默认 feature 仍为空，不自动链接平台适配或 JNI。可按构建选择：

| feature | 内容 |
| --- | --- |
| `builtin-system` | native + 当前平台音视频/图片适配 |
| `builtin-compat-demux` | Windows/macOS 的兼容 demux 与 HTTP/Range |
| `bundled` | 上述两组；在 Android 不编译桌面兼容 demux |
| `builtin-src` | 仅 Android 的可选 standalone SRC |

macOS LAME 保持独立 dylib，未加入 builtin 依赖。Pordo 使用 `bundled`，在自己的 Cargo 依赖图内统一链接，HTTP/TLS 等已有依赖也可共同去重。Windows 应用包不需要 remdia DLL；macOS 应用包只额外带 LAME 扩展与对应清单。

## 独立单库 SDK

`remdia-sdk` 输出 `remdia_sdk.dll` / `libremdia_sdk.dylib` / `libremdia_sdk.so`。其中包括 Engine、内置工厂、公共组件 C ABI，以及 `include/remdia_sdk.h` 声明的作业接口：

- `remdia_sdk_open`：媒体、图片或显式图；输入配置为有界 JSON。
- `remdia_sdk_read`：只读帧/图片/容器输出，EOF 与错误分开。
- `remdia_sdk_seek`：重置媒体图、返回新 generation。
- `remdia_sdk_close`：关闭作业，已经交付的输出继续由其 release 回调持有。

句柄操作必须串行留在创建 worker，取消回调上下文活到作业关闭；输出只能释放一次，可跨线程释放。库本身必须保持加载，直至作业和所有输出都已释放。此接口沿用 remdia.h 的 ABI 2.2 输出结构。它不是将任意 Rust 对象暴露给 C。

Android C 接口使用系统媒体/图片能力前同样需要调用 Java 的 `Remdia.initialize(context)` 初始化桥接。Windows 统一链接版本依赖系统的 Media Foundation 运行库，具体 codec/profile 仍在运行时查询。

桌面构建脚本只构建单库 SDK、薄 CLI 和所需独立扩展。CLI 保持外部宿主模式，避免 SDK 压缩包同时包含两个完整的静态实现。`plugins/sdk.json` 让原有 `Engine::discover` 宿主通过既有组件 ABI 加载统一能力库；默认内置应用则使用 `Engine::bundled*`。

Android AAR 每 ABI 只有 `libremdia_sdk.so`，同时提供既有 Java/JNI 接口。`--system-only` 通过关闭 feature 从单库中排除 SRC，不依赖删除一个 `.so`。`scripts/build-android.py --dynamic-plugins` 可构建旧多库模式作对照；`--dynamic-plugins --system-only` 同样可用。

## 外部扩展和隔离

外部插件继续使用 `remdia_plugin_query_v2`、JSON 控制元数据、平面 retain/release 回调。内置工厂优先于同名外部组件，附加扩展可以注册新的 ID；修改内置实现需要重新链接，不能靠替换一个旧系统插件文件覆盖它。

`Registry::register` 可以接入应用编译的额外 Rust 工厂，`Engine::from_registry` 接收自建注册表。Android 可以把可信、已由 ClassLoader 加载的扩展清单传给 `Engine::bundled_with_preloaded`；Java 初始化也保留这条路径。

内置组件继续保留 panic 边界；发生 panic 后节点标记为不可复用，后续调用报错，析构同样受边界保护。本次没有启用 `panic=abort`，也没有改变失败时退出整个进程的策略。取消回调由包装组件持有的稳定上下文支撑，先销毁内部节点，再释放上下文。

组件 crate 默认只输出 rlib，避免静态依赖还生成没有用途的动态库。独立扩展可用 `cargo rustc -p PACK --lib --crate-type cdylib --release` 构建；该次构建不启用 pack 的 `builtin` feature。不要将被静态链接的 pack 同时当独立插件发行。macOS 也应避免在同一进程同时装入两份包含同名 Objective-C 桥接类的完整 SDK。

## 验证与测量

单元测试验证内置缓冲没有额外复制、取消上下文寿命、输出预算释放和 panic 隔离。`scripts/test-sdk.py --library PATH` 编译独立 C 宿主，验证单库 Engine 的 EOF/seek/取消、关闭后帧所有权和图片图。原有公共组件 ABI C 宿主、原生图测试、网络测试继续运行。

测量见 [统一链接实测](linking-measurements.md)。包体改善不等于全进程内存按相同比例减少；系统 codec/driver 内存不在静态链接去重范围内，集中 LTO 也可能增加单个链接任务的耗时。
