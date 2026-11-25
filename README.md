# Remoters

Mac 和 Windows 共用一套 Rust 桌面客户端。两端都有 **Connect**（主控）和 **Share**（被控）入口，可在同一局域网内通过 IPv4 直连。

## 使用方式

1. 在被控电脑打开 **Share**，选择与主控电脑同网的 IPv4 地址和要共享的显示器，点 **Start sharing**。勾选 **View only** 可只共享画面。
2. 在主控电脑打开 **Connect**，填入被控端显示的地址和配对码，点 **Connect**。默认端口为 `47821`。
3. 点击会话顶部的 **Session** 展开悬浮菜单。要发送鼠标和键盘输入，点击 **Take control**。按 `Ctrl + Alt/Option + Esc` 或移出窗口焦点即可释放输入。
4. 被控端点击 **Stop sharing** 可结束共享。配对码在每次重新开始共享时更新。

macOS 14 或更新版本作为被控端时，需要在系统设置中允许屏幕录制。启用键鼠控制还需要辅助功能权限。两台设备都需要保持登录。连接超时时，检查被控端防火墙是否允许 Remoters 接收局域网 UDP 流量。桌面端的配对、画面和输入共用选定的 UDP 端口（默认 47821）。

两端需要系统提供的 H.264 硬件编解码。Windows 被控端使用 Windows Graphics Capture，需要 Windows 10 1903 或更新版本。

已有的物理和虚拟显示器都会出现在 **Display** 列表中。选择 **New virtual display · 4K** 可创建 3840 × 2160、60 Hz 的虚拟屏，停止共享时移除。macOS 创建虚拟屏依赖 CoreGraphics 的非公开接口。Windows 创建虚拟屏需要已安装、协议兼容的 [SudoVDA](https://github.com/SudoMaker/SudoVDA) 驱动，显示模式需为扩展。如果已经有其他驱动创建的虚拟屏，可以直接选择该屏。

## 当前实现

| 部分 | 代码与职责 |
| --- | --- |
| `crates/core/src/network` | 地址校验、证书指纹和随机配对口令，以及供 WebRTC 使用的 HTTPS 信令。 |
| `crates/core/src/session/quic.rs` | 桌面直连使用 Quinn。视频每帧一个单向流，关键输入走可靠流，鼠标移动走数据报。 |
| `crates/core/src/webrtc` | 保留 str0m 视频轨和 DataChannel，供后续浏览器主控接入。 |
| `crates/core/src/video` | 编码格式、Annex B 码流和访问单元。 |
| `crates/desktop` | Rust + GPUI。Mac、Windows 共用同一个 UI 和主控/被控逻辑。 |
| 平台适配 | macOS 用 ScreenCaptureKit 和 VideoToolbox，Windows 用 Windows Graphics Capture 和 Media Foundation。画面通过 Metal 或 D3D11 交给 GPUI。键鼠通过系统输入接口注入。 |

每次共享使用临时 TLS 身份。配对码包含证书 SHA‑256 指纹和随机口令。主控验证被控端持有对应证书的私钥，然后通过加密连接发送口令。只有认证成功后才会打开桌面会话。

桌面会话使用 H.264，在 4K 范围内保留所选显示器的原生像素尺寸，更大的屏幕按比例缩放。两端均使用原生硬件编码，目标帧率为 60 FPS，默认码率目标随分辨率分为 12、24、45 Mbps。静态桌面只在画面有变化或恢复关键帧时发送。每次会话共享一个显示器，需要两端使用支持 QUIC 的 Remoters 版本。系统保留快捷键、锁屏和系统安全桌面取决于操作系统权限。

## 构建

Windows 或 macOS 使用 Rust 1.92 或更新版本。Windows 需要 MSVC C++ 构建工具和带有 C++/WinRT 标头和 FXC 的 Windows SDK。若 SDK 装在其他目录，构建前将 `GPUI_FXC_PATH` 指向 x64 的 `fxc.exe`。macOS 需要 Xcode 的 Swift 工具链和 macOS SDK。

```sh
cargo test --workspace --locked
cargo build --release --locked -p remoters
```

Windows 的程序在 `target/release/remoters.exe`。在 macOS 上可将构建结果打包为本地签名的应用，以便授予系统权限。

```sh
bash packaging/macos.sh
open target/release/Remoters.app
```

提交和拉取请求会自动构建 [Windows x64](https://github.com/yongtong19/remoters/actions/workflows/desktop.yml)。日常在本机验证 Mac。需要 Apple Silicon 构建产物时，可手动运行 [macOS 工作流](https://github.com/yongtong19/remoters/actions/workflows/macos.yml)。

## 后续阶段

1. **Ubuntu**。复用桌面 UI 和会话，分别适配 X11 和 Wayland 的屏幕采集与输入授权。
2. **Android 主控**。复用 Rust 核心，优先复用界面，按需适配触摸和 Android 生命周期。
3. **跨网连接**。新增独立的公网连接协调服务，交换打洞候选地址。直连失败时使用中转，桌面和输入数据仍由端到端会话加密。局域网直连路径继续独立运行。
